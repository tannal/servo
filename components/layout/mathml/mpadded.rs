//! Requested dimensions and content displacement around an inferred row.
//!
//! See [Core mpadded](https://w3c.github.io/mathml-core/#adjust-space-around-content-mpadded).
//! Its declared geometry can be smaller than the visible contents. Thus measuring
//! fragment rectangles after placement cannot recover the declared baseline/size.
//! Compare [Blink](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/layout/mathml/math_padded_layout_algorithm.cc)
//! and [Gecko](https://searchfox.org/mozilla-central/source/layout/mathml/nsMathMLmpaddedFrame.cpp).
//! This path currently parses a limited length-percentage representation; it should
//! not be read as supporting every historical mpadded pseudo-unit or syntax.

use app_units::Au;
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::attr::AttrValue;
use style::context::{SharedStyleContext, TreeCountingCaches};
use style::dom::DummyElementContext;
use style::properties::{ComputedValues, StyleBuilder};
use style::rule_cache::RuleCacheConditions;
use style::rule_tree::RuleCascadeFlags;
use style::stylesheets::container_rule::ContainerSizeQuery;
use style::values::computed::{Context, LengthPercentage, ToComputedValue};
use web_atoms::local_name;
use web_atoms::ns;

use super::MathLevelBox;
use crate::cell::ArcRefCell;
use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::dom_traversal::NodeAndStyleInfo;
use crate::fragment_tree::BaseFragmentInfo;
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{
    MathBoxMetrics, MathLayoutResult, MathRowBlock, MathStretchTarget, translate_fragments,
};
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult};
use crate::style_ext::LayoutStyle;
use crate::{ConstraintSpace, ContainingBlock};
use style::Zero;

/// One styled wrapper around inferred-row contents, with independent declared
/// geometry and content displacement. A fragment's painted bounds cannot replace
/// these declared metrics when the wrapper intentionally permits overflow.
#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathPaddedBlock {
    /// The single styled wrapper; the inferred row must not apply these decorations again.
    pub base: LayoutBoxBase,
    /// Inner mrow container used for initial layout & metric resolution
    /// Undecorated inferred-row measurement and child coordination.
    pub inner_row: MathRowBlock,

    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    /// Requested inline content displacement; does not by itself increase declared width.
    pub lspace: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    /// Requested baseline-relative content displacement; positive moves content upward.
    pub voffset: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    /// Optional declared inline size override, independent of overflowing contents.
    pub width: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    /// Optional declared ascent override; not recovered from child fragment origins.
    pub height: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    /// Optional declared descent override; separate from content displacement.
    pub depth: Option<LengthPercentage>,
}

impl MathPaddedBlock {
    /// Build the inferred row and compute the supported attribute values using
    /// the element's style. Keep them separate from the eventual child measurements.
    pub(crate) fn construct(
        context: &LayoutContext,
        node_and_style_info: &NodeAndStyleInfo,
        children: Vec<ArcRefCell<MathLevelBox>>,
    ) -> MathPaddedBlock {
        let base_fragment_info: BaseFragmentInfo = node_and_style_info.into();
        let base = LayoutBoxBase::new(base_fragment_info, node_and_style_info.style.clone());

        let element = node_and_style_info.node.as_element();

        // 1. 构建 Stylo ToComputedContext 解析 mpadded 属性
        let rule_cache_conditions = &mut RuleCacheConditions::default();
        let mut tree_counting_caches = TreeCountingCaches::default();
        let style_builder = StyleBuilder::new(
            context.style_context.stylist.device(),
            Some(context.style_context.stylist),
            Some(&node_and_style_info.style),
            None,
            None,
            false,
        );

        let to_computed_context = Context::new(
            style_builder,
            context.style_context.quirks_mode(),
            rule_cache_conditions,
            ContainerSizeQuery::none(),
            RuleCascadeFlags::empty(),
            &DummyElementContext,
            &mut tree_counting_caches,
        );

        let parse_attr = |name| {
            element
                .as_ref()
                .and_then(|el| el.attribute(&ns!(), &name))
                .and_then(|attr| match attr {
                    AttrValue::LengthPercentage(_, lp) => lp.as_ref(),
                    _ => None,
                })
                .map(|lp| lp.to_computed_value(&to_computed_context))
        };

        let lspace = parse_attr(local_name!("lspace"));
        let voffset = parse_attr(web_atoms::LocalName::from("voffset"));
        let width = parse_attr(local_name!("width"));
        let height = parse_attr(local_name!("height"));
        let depth = parse_attr(local_name!("depth"));

        // 2. 根据传入的 children 计算 Layout Subtree Size
        let subtree_size = children
            .iter()
            .map(|c| c.borrow().with_base(|base| base.subtree_size()))
            .sum::<usize>()
            + 1;
        base.set_subtree_size(subtree_size);

        let inner_row = MathRowBlock::construct(node_and_style_info, children);

        MathPaddedBlock {
            base,
            inner_row,
            lspace,
            voffset,
            width,
            height,
            depth,
        }
    }

    /// Repair wrapper/row styles. This existing path does not currently reparse
    /// mpadded attributes; documentation must not imply dynamic attribute support.
    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
        self.inner_row.repair_style(context, node, new_style);

        let rule_cache_conditions = &mut RuleCacheConditions::default();
        let mut tree_counting_caches = TreeCountingCaches::default();
        let style_builder = StyleBuilder::new(
            context.stylist.device(),
            Some(context.stylist),
            Some(new_style),
            None,
            None,
            false,
        );
        let to_computed_context = Context::new(
            style_builder,
            context.quirks_mode(),
            rule_cache_conditions,
            ContainerSizeQuery::none(),
            RuleCascadeFlags::empty(),
            &DummyElementContext,
            &mut tree_counting_caches,
        );
        let element = node.as_element();
        let parse_attr = |name| {
            element
                .as_ref()
                .and_then(|el| el.attribute(&ns!(), &name))
                .and_then(|attr| match attr {
                    AttrValue::LengthPercentage(_, lp) => lp.as_ref(),
                    _ => None,
                })
                .map(|lp| lp.to_computed_value(&to_computed_context))
        };
        self.lspace = parse_attr(local_name!("lspace"));
        self.voffset = parse_attr(web_atoms::LocalName::from("voffset"));
        self.width = parse_attr(local_name!("width"));
        self.height = parse_attr(local_name!("height"));
        self.depth = parse_attr(local_name!("depth"));
    }

    /// Forward descendant ownership through the inferred row.
    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        self.inner_row.attached_to_tree(layout_box);
    }

    #[inline]
    /// Resolve the stored length-percentage against the basis selected by the
    /// caller; this helper does not implement historical height/depth pseudo-units.
    fn resolve_length(&self, length: &Option<LengthPercentage>, base_val: Au) -> Option<Au> {
        length.as_ref().map(|lp| lp.to_used_value(base_val))
    }

    /// Measure the inferred row without wrapper decorations, apply a supported
    /// width override, and decorate once. Content displacement does not expand width.
    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.base.cached_inline_content_sizes_with(constraint_space, || {
            let inner_sizes = self
                .inner_row
                .content_inline_sizes(layout_context, constraint_space);

            let mut min_content = inner_sizes.sizes.min_content;
            let mut max_content = inner_sizes.sizes.max_content;

            if let Some(w) = self.resolve_length(&self.width, max_content) {
                min_content = w.max(Au::zero());
                max_content = w.max(Au::zero());
            }

            let pbm = LayoutStyle::Default(&self.base.style)
                .content_box_sizes_and_padding_border_margin(&constraint_space.into())
                .pbm;
            let extra =
                pbm.padding_border_sums.inline + pbm.margin.auto_is(Au::zero).inline_sum();
            min_content += extra;
            max_content += extra;

            InlineContentSizesResult {
                sizes: ContentSizes {
                    min_content,
                    max_content,
                },
                depends_on_block_constraints: inner_sizes.depends_on_block_constraints,
            }
        })
    }

    /// Read inferred-row width/ascent/descent directly, apply supported declared
    /// size overrides, and translate the children relative to the declared baseline.
    /// Positive voffset moves the content upward without increasing declared height.
    /// The RTL branch below is the current horizontal mapping, not general orthogonal
    /// writing-mode support. Finish only the mpadded wrapper, not an extra styled row.
    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
        let style = &self.base.style;
        let is_rtl = !style.writing_mode.is_bidi_ltr();

        let layout_style = LayoutStyle::Default(style);
        let pbm = layout_style
            .content_box_sizes_and_padding_border_margin(&containing_block.into())
            .pbm;
        // Use the anonymous row's measurements directly. Fragment origins are
        // positions, not ascents, and cannot reconstruct the baseline contract.
        let output = self.inner_row.layout_contents(
            layout_context,
            positioning_context,
            containing_block,
            target_size,
        );
        let inner_width = output.metrics.size.inline;
        let inner_ascent = output.metrics.ascent();
        let inner_descent = output.metrics.descent();
        let mut child_fragments = output.fragments;

        // 3. Resolve attribute metric overrides
        let resolved_width = self
            .resolve_length(&self.width, inner_width)
            .unwrap_or(inner_width)
            .max(Au::zero());

        let resolved_ascent = self
            .resolve_length(&self.height, inner_ascent)
            .unwrap_or(inner_ascent)
            .max(Au::zero());

        let resolved_descent = self
            .resolve_length(&self.depth, inner_descent)
            .unwrap_or(inner_descent)
            .max(Au::zero());

        let lspace = self
            .resolve_length(&self.lspace, Au::zero())
            .unwrap_or_default();

        let voffset = self
            .resolve_length(&self.voffset, Au::zero())
            .unwrap_or_default();

        // 4. Calculate positioning offsets
        let offset_x = if is_rtl {
            resolved_width - inner_width - lspace
        } else {
            lspace
        };

        // Preserve the requested baseline while moving the contents upwards
        // for positive voffset. Height/depth overrides need not enclose the ink.
        let offset_y = resolved_ascent - inner_ascent - voffset;
        translate_fragments(&mut child_fragments, offset_x, offset_y);

        MathLayoutResult::from_content(
            &self.base,
            containing_block,
            &pbm,
            child_fragments,
            MathBoxMetrics::new(
                resolved_width,
                resolved_ascent + resolved_descent,
                resolved_ascent,
            ),
            None,
        )
    }
}
