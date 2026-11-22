//! Rows coordinate their children and align returned measurements.
//!
//! See [Core rows](https://w3c.github.io/mathml-core/#group-sub-expressions-mrow)
//! and the [block stretching algorithm](https://w3c.github.io/mathml-core/#algorithm-for-stretching-operators-along-the-block-axis).
//! Compare [Blink rows](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/layout/mathml/math_row_layout_algorithm.cc),
//! [WebKit rows](https://github.com/WebKit/WebKit/blob/main/Source/WebCore/rendering/mathml/RenderMathMLRow.cpp), and
//! [Typst fences](https://github.com/typst/typst/blob/main/crates/typst-layout/src/math/fenced.rs).
//! They demonstrate measuring surrounding material before sizing delimiters, but
//! are not identical: WebKit's fallback minimum is not Core's zero-target procedure.
//! TeX's Appendix G rule 19 also follows the measurement-before-delimiter pattern;
//! its delimiter factor and shortfall are not MathML stretch constraints.
//!
//! This implementation currently sums advances without the complete italic-
//! correction/operator-spacing algorithm. Out-of-flow children are excluded from
//! row measurements, but running their layout is not a complete CSS hoisting path.

use app_units::Au;
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::context::SharedStyleContext;
use style::properties::ComputedValues;
use web_atoms::local_name;

use super::MathLevelBox;
use crate::cell::ArcRefCell;
use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::dom_traversal::NodeAndStyleInfo;
use crate::fragment_tree::BaseFragmentInfo;
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{
    MathBoxMetrics, MathLayoutCache, MathLayoutResult, MathOperatorProperties, MathStretchAxis,
    MathStretchTarget, translate_fragments,
};
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult};
use crate::style_ext::LayoutStyle;
use crate::{ConstraintSpace, ContainingBlock};
use style::Zero;

/// Controls which recursive structural query is valid for a box currently
/// represented by a row. This is identity policy, not the glyph-stretch axis.
#[derive(Debug, MallocSizeOf, PartialEq)]
enum MathRowKind {
    /// One embellished child plus space-like siblings may define the core operator.
    Grouping,
    /// Fraction/script fallback follows its first in-flow child for operator identity.
    FirstChildOperator,
    /// Radical fallback must not acquire operator identity merely from row layout.
    Other,
}

/// Source-ordered mathematical child list and its styled outer box. Content-only
/// measurement/layout helpers allow an inferred row to share child coordination
/// without creating another CSS decoration layer.
#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathRowBlock {
    /// Style, node identity, and fragment bookkeeping for the outer box.
    pub base: LayoutBoxBase,
    /// Children in source order; their accepted results are placed in this order.
    pub children: Vec<ArcRefCell<MathLevelBox>>,
    /// Structural classification independent of the temporary row layout fallback.
    kind: MathRowKind,
    /// True if this node is an `<mprescripts>` separator inside `<mmultiscripts>`.
    pub is_mprescripts: bool,
    /// Incremental layout result cache.
    layout_cache: MathLayoutCache,
}
impl MathRowBlock {
    /// Preserve source order and select structural operator-identity rules.
    pub(crate) fn construct(
        node_and_style_info: &NodeAndStyleInfo,
        children: Vec<ArcRefCell<MathLevelBox>>,
    ) -> MathRowBlock {
        let base_fragment_info: BaseFragmentInfo = node_and_style_info.into();
        let base = LayoutBoxBase::new(base_fragment_info, node_and_style_info.style.clone());
        let subtree_size = children
            .iter()
            .map(|c| c.borrow().with_base(|base| base.subtree_size()))
            .sum::<usize>()
            + 1;
        base.set_subtree_size(subtree_size);
        let is_mprescripts = node_and_style_info
            .node
            .as_element()
            .is_some_and(|element| element.local_name() == &local_name!("mprescripts"));
        let kind = node_and_style_info
            .node
            .as_element()
            .map_or(MathRowKind::Grouping, |element| {
                match element.local_name() {
                    &local_name!("msqrt") | &local_name!("mroot") => MathRowKind::Other,
                    &local_name!("msub")
                    | &local_name!("msup")
                    | &local_name!("msubsup")
                    | &local_name!("munder")
                    | &local_name!("mover")
                    | &local_name!("munderover")
                    | &local_name!("mmultiscripts")
                    | &local_name!("mfrac") => MathRowKind::FirstChildOperator,
                    _ => MathRowKind::Grouping,
                }
            });
        MathRowBlock {
            base,
            children,
            kind,
            is_mprescripts,
            layout_cache: MathLayoutCache::default(),
        }
    }

    pub(crate) fn repair_style(
        &mut self,
        _context: &SharedStyleContext,
        _node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
        self.layout_cache.clear();
    }

    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        for child in &self.children {
            child.borrow().attached_to_tree(layout_box.clone());
        }
    }

    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.base.cached_inline_content_sizes_with(constraint_space, || {
            let mut result = self.content_inline_sizes(layout_context, constraint_space);
            let pbm = LayoutStyle::Default(&self.base.style)
                .content_box_sizes_and_padding_border_margin(&constraint_space.into())
                .pbm;
            let extra =
                pbm.padding_border_sums.inline + pbm.margin.auto_is(Au::zero).inline_sum();
            result.sizes.min_content += extra;
            result.sizes.max_content += extra;
            result
        })
    }

    /// Sum in-flow contributions for the inferred row. This is currently the
    /// advance-only part of the row algorithm: full operator spacing and italic
    /// correction still need integration. Keeping this undecorated lets mpadded reuse it.
    pub(crate) fn content_inline_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        let mut min_content = Au::zero();
        let mut max_content = Au::zero();
        let mut depends_on_block_constraints = false;
        for child in &self.children {
            if !child.borrow().is_in_flow() {
                continue;
            }
            let child_sizes = child
                .borrow()
                .inline_content_sizes(layout_context, constraint_space);
            min_content += child_sizes.sizes.min_content;
            max_content += child_sizes.sizes.max_content;
            depends_on_block_constraints |= child_sizes.depends_on_block_constraints;
        }
        InlineContentSizesResult {
            sizes: ContentSizes {
                min_content,
                max_content,
            },
            depends_on_block_constraints,
        }
    }
    /// Locate the unique embellished child, ignoring only in-flow space-like
    /// siblings. This query does not perform layout or retain a borrowed child.
    /// Grouping rows ignore space-like siblings and require one remaining operator.
    /// First-child containers stop at their first in-flow child, even if a later
    /// child would qualify. `None` means no core operator, not zero dimensions.
    /// See <https://w3c.github.io/mathml-core/#embellished-operators>.
    pub(crate) fn embellished_operator_index(&self) -> Option<usize> {
        if self.kind == MathRowKind::Other {
            return None;
        }
        if self.kind == MathRowKind::FirstChildOperator {
            return self
                .children
                .iter()
                .enumerate()
                .find_map(|(index, child)| {
                    let child = child.borrow();
                    child
                        .is_in_flow()
                        .then(|| child.operator_properties().map(|_| index))
                })
                .flatten();
        }
        let mut operator = None;
        for (index, child) in self.children.iter().enumerate() {
            let child = child.borrow();
            if !child.is_in_flow() || child.is_space_like() {
                continue;
            }
            if operator.is_some() || child.operator_properties().is_none() {
                return None;
            }
            operator = Some(index);
        }
        operator
    }

    /// Follow the selected child and copy only properties needed by the layout
    /// scheduler. Do not infer capability from the most recently returned rectangle.
    pub(crate) fn operator_properties(&self) -> Option<MathOperatorProperties> {
        let index = self.embellished_operator_index()?;
        self.children[index].borrow().operator_properties()
    }

    /// Apply recursive structural classification to in-flow children. An empty
    /// group qualifies; a nonempty mtext can qualify while still occupying space.
    /// See <https://w3c.github.io/mathml-core/#definition-of-space-like-elements>.
    pub(crate) fn is_space_like(&self) -> bool {
        self.kind == MathRowKind::Grouping
            && self.children.iter().all(|child| {
                let child = child.borrow();
                !child.is_in_flow() || child.is_space_like()
            })
    }

    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
        if let Some(cached) = self.layout_cache.get(
            &self.base,
            containing_block,
            target_size,
            positioning_context,
        ) {
            return cached;
        }
        let mut local_positioning = PositioningContext::default();
        let pbm = LayoutStyle::Default(&self.base.style)
            .content_box_sizes_and_padding_border_margin(&containing_block.into())
            .pbm;
        let content = self.layout_contents(
            layout_context,
            &mut local_positioning,
            containing_block,
            target_size,
        );
        let result = MathLayoutResult::from_content(
            &self.base,
            containing_block,
            &pbm,
            content.fragments,
            content.metrics,
            None,
        );
        self.layout_cache.store(
            &self.base,
            containing_block,
            target_size,
            &result,
            &local_positioning,
        );
        positioning_context.append(local_positioning);
        result
    }

    pub(crate) fn layout_contents(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
        let inherited_operator = target_size.and_then(|_| self.embellished_operator_index());
        let properties: Vec<_> = self
            .children
            .iter()
            .map(|child| child.borrow().operator_properties())
            .collect();
        let mut outputs: Vec<Option<MathLayoutResult>> =
            (0..self.children.len()).map(|_| None).collect();
        let mut deferred = Vec::new();
        let mut target_extents: Option<(Au, Au)> = None;

        for (index, child) in self.children.iter().enumerate() {
            let child = child.borrow();
            if inherited_operator.is_none()
                && child.is_in_flow()
                && properties[index].is_some_and(|properties| {
                    properties.stretchy && properties.stretch_axis == MathStretchAxis::Block
                })
            {
                deferred.push(index);
                continue;
            }
            let target = if inherited_operator == Some(index) {
                target_size
            } else {
                None
            };
            let output = child.layout_for_math_parent(
                layout_context,
                positioning_context,
                containing_block,
                &self.base.style,
                target,
            );
            if child.is_in_flow() {
                let ink = (output.metrics.ink_ascent, output.metrics.ink_descent);
                target_extents = Some(target_extents.map_or(ink, |previous| {
                    (previous.0.max(ink.0), previous.1.max(ink.1))
                }));
            }
            outputs[index] = Some(output);
        }

        if target_extents.is_none() {
            for &index in &deferred {
                let mut measurement_positioning = PositioningContext::default();
                let measurement = self.children[index].borrow().layout_for_math_parent(
                    layout_context,
                    &mut measurement_positioning,
                    containing_block,
                    &self.base.style,
                    Some(MathStretchTarget::Block {
                        ascent: Au::zero(),
                        descent: Au::zero(),
                    }),
                );
                let ink = (
                    measurement.metrics.ink_ascent,
                    measurement.metrics.ink_descent,
                );
                target_extents = Some(target_extents.map_or(ink, |previous| {
                    (previous.0.max(ink.0), previous.1.max(ink.1))
                }));
            }
        }
        let (target_ascent, target_descent) = target_extents.unwrap_or((Au::zero(), Au::zero()));
        let target = Some(MathStretchTarget::Block {
            ascent: target_ascent,
            descent: target_descent,
        });
        for index in deferred {
            outputs[index] = Some(self.children[index].borrow().layout_for_math_parent(
                layout_context,
                positioning_context,
                containing_block,
                &self.base.style,
                target,
            ));
        }

        let mut extents: Option<(Au, Au, Au, Au)> = None;
        let mut total_inline = Au::zero();
        for (child, output) in self.children.iter().zip(&outputs) {
            if !child.borrow().is_in_flow() {
                continue;
            }
            let metrics = &output.as_ref().unwrap().metrics;
            total_inline += metrics.size.inline;
            let child = (
                metrics.ascent(),
                metrics.descent(),
                metrics.ink_ascent,
                metrics.ink_descent,
            );
            extents = Some(extents.map_or(child, |previous| {
                (
                    previous.0.max(child.0),
                    previous.1.max(child.1),
                    previous.2.max(child.2),
                    previous.3.max(child.3),
                )
            }));
        }
        let (ascent, descent, ink_ascent, ink_descent) =
            extents.unwrap_or((Au::zero(), Au::zero(), Au::zero(), Au::zero()));
        let is_rtl = !self.base.style.writing_mode.is_bidi_ltr();
        let mut fragments = Vec::new();
        let mut inline_cursor = if is_rtl { total_inline } else { Au::zero() };
        for (child, output) in self.children.iter().zip(outputs) {
            let mut output = output.unwrap();
            if child.borrow().is_in_flow() {
                let x = if is_rtl {
                    inline_cursor -= output.metrics.size.inline;
                    inline_cursor
                } else {
                    let pos = inline_cursor;
                    inline_cursor += output.metrics.size.inline;
                    pos
                };
                translate_fragments(&mut output.fragments, x, ascent - output.metrics.ascent());
            }
            fragments.extend(output.fragments);
        }
        let mut metrics = MathBoxMetrics::new(total_inline, ascent + descent, ascent);
        metrics.ink_ascent = ink_ascent;
        metrics.ink_descent = ink_descent;
        MathLayoutResult { fragments, metrics }
    }
}
