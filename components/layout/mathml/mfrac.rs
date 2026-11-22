/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Fraction layout separates font policy, clearance constraints, and placement.
//!
//! [Core fractions](https://w3c.github.io/mathml-core/#fractions-mfrac) are the
//! behavior target. Historical comparison: The TeXbook Appendix G, pp. 444-445,
//! rules 15a-e, and TeX's `make_fraction` use measured numerator/denominator boxes,
//! preferred shifts, then clearance corrections. TeX's ruled-fraction clearance
//! uses actual rule thickness; Core's fallback constants are resolved separately.
//! Do not substitute one policy for the other just because the formulas look alike.
//!
//! Compare [Blink](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/layout/mathml/math_fraction_layout_algorithm.cc),
//! [Gecko](https://searchfox.org/mozilla-central/source/layout/mathml/nsMathMLmfracFrame.cpp),
//! [WebKit](https://github.com/WebKit/WebKit/blob/main/Source/WebCore/rendering/mathml/RenderMathMLFraction.cpp),
//! [Typst](https://github.com/typst/typst/blob/main/crates/typst-layout/src/math/fraction.rs), and
//! [KaTeX](https://github.com/KaTeX/KaTeX/blob/main/src/functions/genfrac.ts).
//! Those files differ in legacy options, fallback policy, and box representation;
//! these links are comparisons rather than a claim of identical behavior.

use app_units::Au;
use fonts::Font;
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::Zero;
use style::attr::AttrValue;
use style::context::{SharedStyleContext, TreeCountingCaches};
use style::dom::DummyElementContext;
use style::logical_geometry::LogicalAxis;
use style::properties::{ComputedValues, StyleBuilder};
use style::rule_cache::RuleCacheConditions;
use style::rule_tree::RuleCascadeFlags;
use style::stylesheets::container_rule::ContainerSizeQuery;
use style::values::computed::font::MathStyle;
use style::values::computed::{Context, LengthPercentage, ToComputedValue};
use web_atoms::{local_name, ns};

use super::{MathLevelBox, MathRowBlock};
use crate::cell::ArcRefCell;
use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::dom_traversal::NodeAndStyleInfo;
use crate::fragment_tree::MathPaintInfo;
use crate::geom::{PhysicalPoint, PhysicalRect, PhysicalSize};
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{
    MathBoxMetrics, MathLayoutCache, MathLayoutResult, MathStretchTarget, translate_fragments,
};
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult, Size};
use crate::style_ext::LayoutStyle;
use crate::containing_block::{ConstraintSpace, ContainingBlock};

/// Baseline shifts after enforcing the font's minimum clearances.
#[derive(Debug, Clone, Copy)]
struct FractionShifts {
    /// Positive distance moving the numerator baseline toward line-over.
    numerator_shift_up: Au,
    /// Positive distance moving the denominator baseline toward line-under.
    denominator_shift_down: Au,
}

/// Font-dependent inputs shared by fraction and stack layout.
#[derive(Debug, Clone, Copy)]
struct FractionMetrics {
    /// Alphabetic-baseline to rule-center distance (AxisHeight), in scaled Au.
    axis_height: Au,
    /// Font default used before resolving the author's linethickness, including percentages.
    default_thickness: Au,
    /// Preferred numerator baseline shift for a ruled fraction; clearance may increase it.
    numerator_min_shift_up: Au,
    /// Preferred denominator baseline shift for a ruled fraction; clearance may increase it.
    denominator_min_shift_down: Au,
    /// Minimum clearance from numerator ink to the rule's upper edge.
    numerator_gap_min: Au,
    /// Minimum clearance from the rule's lower edge to denominator ink.
    denominator_gap_min: Au,
    /// Preferred upper baseline shift for a zero-rule stack.
    stack_top_shift_up: Au,
    /// Preferred lower baseline shift for a zero-rule stack.
    stack_bottom_shift_down: Au,
    /// Minimum combined inter-child clearance when there is no fraction rule.
    stack_gap_min: Au,
}

impl FractionMetrics {
    /// Resolve scaled font constants once, independently of author linethickness.
    /// https://w3c.github.io/mathml-core/#layout-constants-mathconstants
    /// Select normal/compact constants from one font instance. Font access already
    /// scales values into Au. A zero constant is valid and must not trigger fallback.
    /// The no-MATH path initializes lazy ordinary metrics; those platform metrics can
    /// be synthesized, so this is not a guarantee of exact raw OS/2/post table values.
    fn new(font: Option<&Font>, is_display_style: bool) -> Self {
        if let Some(constants) = font.and_then(|font| font.math_constants(is_display_style)) {
            return Self {
                axis_height: constants.axis_height,
                default_thickness: constants.rule_thickness,
                numerator_min_shift_up: constants.num_shift_up,
                denominator_min_shift_down: constants.denom_shift_down,
                numerator_gap_min: constants.num_gap_min,
                denominator_gap_min: constants.denom_gap_min,
                stack_top_shift_up: constants.stack_top_shift_up,
                stack_bottom_shift_down: constants.stack_bottom_shift_down,
                stack_gap_min: constants.stack_gap_min,
            };
        }

        let (axis_height, default_thickness) = font
            .map(|font| {
                // Initialize lazy metrics rather than depending on whether some
                // earlier operation happened to populate the font's cache.
                let metrics = font.metrics();
                (metrics.x_height / 2, metrics.underline_size.max(Au::zero()))
            })
            .unwrap_or((Au::zero(), Au::zero()));
        let fraction_gap = default_thickness * if is_display_style { 3 } else { 1 };

        Self {
            axis_height,
            default_thickness,
            numerator_min_shift_up: Au::zero(),
            denominator_min_shift_down: Au::zero(),
            numerator_gap_min: fraction_gap,
            denominator_gap_min: fraction_gap,
            stack_top_shift_up: Au::zero(),
            stack_bottom_shift_down: Au::zero(),
            stack_gap_min: default_thickness * if is_display_style { 7 } else { 3 },
        }
    }

    /// Solve baseline constraints in signed baseline-relative coordinates.
    ///
    /// For a rule: U >= axis + t/2 + numerator_gap + numerator_descent,
    /// and D >= t/2 + denominator_gap + denominator_ascent - axis. Each
    /// preferred shift is independently increased until its inequality holds.
    ///
    /// For a stack: gap = U - numerator_descent + D - denominator_ascent.
    /// Split a positive deficit between U and D, preserving the odd-Au remainder.
    /// The input extents are the children's clearance measurements, not font ascents.
    ///
    /// See [Core ruled fractions](https://w3c.github.io/mathml-core/#fraction-with-nonzero-line-thickness)
    /// and [stacks](https://w3c.github.io/mathml-core/#fraction-with-zero-line-thickness).
    /// TeXbook Appendix G rules 15c-d have the same constraint-solving shape; their
    /// font parameters and thickness-dependent spacing policy are not substituted here.
    fn compute_shifts(
        &self,
        thickness: Au,
        numerator_descent: Au,
        denominator_ascent: Au,
    ) -> FractionShifts {
        if thickness > Au::zero() {
            let half_thickness = thickness / 2;
            let numerator_clearance =
                self.axis_height + half_thickness + self.numerator_gap_min + numerator_descent;
            let denominator_clearance =
                half_thickness + self.denominator_gap_min + denominator_ascent - self.axis_height;

            return FractionShifts {
                numerator_shift_up: self.numerator_min_shift_up.max(numerator_clearance),
                denominator_shift_down: self.denominator_min_shift_down.max(denominator_clearance),
            };
        }

        let mut numerator_shift_up = self.stack_top_shift_up;
        let mut denominator_shift_down = self.stack_bottom_shift_down;
        let actual_gap =
            numerator_shift_up - numerator_descent + denominator_shift_down - denominator_ascent;

        if actual_gap < self.stack_gap_min {
            let deficit = self.stack_gap_min - actual_gap;
            let numerator_adjustment = deficit / 2;
            numerator_shift_up += numerator_adjustment;
            // Preserve the remainder when the deficit is an odd number of Au.
            denominator_shift_down += deficit - numerator_adjustment;
        }

        FractionShifts {
            numerator_shift_up,
            denominator_shift_down,
        }
    }
}

/// Box-tree state for a fraction, not a cached measurement. Numerator and
/// denominator ownership is distinct from the fallback row's child list. Strip
/// neither CSS styling from the wrapper nor mathematical paint data from its result.
#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathFractionBlock {
    /// Owns the styled base and fallback children; valid fractions retain out-of-flow children here.
    pub inner_row: MathRowBlock,
    /// First in-flow child when arity is valid; paired with denominator.
    pub numerator: Option<ArcRefCell<MathLevelBox>>,
    /// Second in-flow child when arity is valid; never receives the parent stretch target.
    pub denominator: Option<ArcRefCell<MathLevelBox>>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    /// Computed attribute value; percentages wait for the scaled default rule thickness.
    pub linethickness: Option<LengthPercentage>,
    /// Incremental layout result cache.
    layout_cache: MathLayoutCache,
}

impl MathFractionBlock {
    /// Partition children only after counting in-flow items. A valid fraction owns
    /// two measured children; otherwise retain original order for row fallback.
    pub(crate) fn construct(
        context: &LayoutContext,
        node_and_style_info: &NodeAndStyleInfo,
        children: Vec<ArcRefCell<MathLevelBox>>,
    ) -> Self {
        let in_flow_indices: Vec<_> = children
            .iter()
            .enumerate()
            .filter_map(|(index, child)| {
                let is_in_flow = child
                    .borrow()
                    .with_base(|base| !base.style.get_box().position.is_absolutely_positioned());

                is_in_flow.then_some(index)
            })
            .collect();

        if in_flow_indices.len() != 2 {
            return Self {
                inner_row: MathRowBlock::construct(node_and_style_info, children),
                numerator: None,
                denominator: None,
                linethickness: None,
                layout_cache: MathLayoutCache::default(),
            };
        }

        let linethickness = Self::resolve_linethickness_attr(
            node_and_style_info.node.as_element(),
            &context.style_context,
            &node_and_style_info.style,
        );

        let numerator_index = in_flow_indices[0];
        let denominator_index = in_flow_indices[1];
        let mut numerator = None;
        let mut denominator = None;
        let mut out_of_flow = Vec::new();

        for (index, child) in children.into_iter().enumerate() {
            if index == numerator_index {
                numerator = Some(child);
            } else if index == denominator_index {
                denominator = Some(child);
            } else {
                out_of_flow.push(child);
            }
        }

        Self {
            inner_row: MathRowBlock::construct(node_and_style_info, out_of_flow),
            numerator,
            denominator,
            linethickness,
            layout_cache: MathLayoutCache::default(),
        }
    }

    /// Read the DOM's parsed length-percentage and compute font-relative units using
    /// the fraction's own style. Do not resolve percentages until font default rule
    /// thickness is known. Creating this temporary style context is not a font cache.
    fn resolve_linethickness_attr<'dom, E>(
        element: Option<E>,
        context: &SharedStyleContext,
        style: &ServoArc<ComputedValues>,
    ) -> Option<LengthPercentage>
    where
        E: LayoutElement<'dom>,
    {
        let binding = element?;
        let attr = binding.attribute(&ns!(), &local_name!("linethickness"))?;
        let specified = match attr {
            AttrValue::LengthPercentage(_, lp) => lp.as_ref()?,
            _ => return None,
        };
        let mut rule_cache_conditions = RuleCacheConditions::default();
        let mut tree_counting_caches = TreeCountingCaches::default();
        let style_builder = StyleBuilder::new(
            context.stylist.device(),
            Some(context.stylist),
            Some(style),
            None,
            None,
            false,
        );
        let computed_context = Context::new(
            style_builder,
            context.quirks_mode(),
            &mut rule_cache_conditions,
            ContainerSizeQuery::none(),
            RuleCascadeFlags::empty(),
            &DummyElementContext,
            &mut tree_counting_caches,
        );
        Some(specified.to_computed_value(&computed_context))
    }

    /// Borrow the styled base owned by the inner row; the fraction has one CSS box.
    pub(crate) fn base(&self) -> &LayoutBoxBase {
        &self.inner_row.base
    }

    /// Mutable access to that same base for existing box-tree lifecycle operations.
    pub(crate) fn base_mut(&mut self) -> &mut LayoutBoxBase {
        &mut self.inner_row.base
    }

    #[inline]
    /// Whether construction supplied both required in-flow operands.
    pub(crate) fn is_valid(&self) -> bool {
        self.fraction_children().is_some()
    }

    /// Return operands as a pair so layout cannot proceed with only one operand.
    fn fraction_children(&self) -> Option<(&ArcRefCell<MathLevelBox>, &ArcRefCell<MathLevelBox>)> {
        self.numerator.as_ref().zip(self.denominator.as_ref())
    }

    /// Percentages resolve relative to the default FractionRuleThickness.
    /// Clamp the resolved author value to zero. Absent/invalid attributes retain
    /// the font default; an explicit zero selects stack layout. The default thickness
    /// also remains distinct from gap constants derived during font resolution.
    pub(crate) fn resolve_line_thickness(&self, default_thickness: Au) -> Au {
        match &self.linethickness {
            Some(value) => value.to_used_value(default_thickness).max(Au::zero()),
            None => default_thickness,
        }
    }

    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.inner_row.repair_style(context, node, new_style);
        self.linethickness =
            Self::resolve_linethickness_attr(node.as_element(), context, new_style);
        self.layout_cache.clear();
    }

    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        if let Some(ref num) = self.numerator {
            num.borrow().attached_to_tree(layout_box.clone());
        }
        if let Some(ref den) = self.denominator {
            den.borrow().attached_to_tree(layout_box.clone());
        }
        self.inner_row.attached_to_tree(layout_box);
    }

    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.base().cached_inline_content_sizes_with(constraint_space, || {
            let Some((numerator, denominator)) = self.fraction_children() else {
                return self
                    .inner_row
                    .inline_content_sizes(layout_context, constraint_space);
            };

            let numerator_sizes = numerator
                .borrow()
                .inline_content_sizes(layout_context, constraint_space);
            let denominator_sizes = denominator
                .borrow()
                .inline_content_sizes(layout_context, constraint_space);

            let pbm = LayoutStyle::Default(&self.base().style)
                .content_box_sizes_and_padding_border_margin(&constraint_space.into())
                .pbm;
            let extra =
                pbm.padding_border_sums.inline + pbm.margin.auto_is(Au::zero).inline_sum();

            InlineContentSizesResult {
                sizes: ContentSizes {
                    min_content: numerator_sizes
                        .sizes
                        .min_content
                        .max(denominator_sizes.sizes.min_content)
                        + extra,
                    max_content: numerator_sizes
                        .sizes
                        .max_content
                        .max(denominator_sizes.sizes.max_content)
                        + extra,
                },
                depends_on_block_constraints: numerator_sizes.depends_on_block_constraints
                    || denominator_sizes.depends_on_block_constraints,
            }
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
            self.base(),
            containing_block,
            target_size,
            positioning_context,
        ) {
            return cached;
        }

        let Some((numerator, denominator)) = self.fraction_children() else {
            return self.inner_row.layout(
                layout_context,
                positioning_context,
                containing_block,
                target_size,
            );
        };

        let mut local_positioning = PositioningContext::default();
        let style = &self.base().style;
        let writing_mode = style.writing_mode;
        let layout_style = LayoutStyle::Default(style);
        let box_sizes =
            layout_style.content_box_sizes_and_padding_border_margin(&containing_block.into());
        let pbm = box_sizes.pbm;
        let physical_padding = pbm.padding.to_physical(writing_mode);
        let physical_border = pbm.border.to_physical(writing_mode);

        // An incoming constraint belongs only to the embellished numerator.
        let numerator = numerator.borrow();
        let stretch = target_size.filter(|_| numerator.operator_properties().is_some());
        let mut numerator_output = numerator.layout_for_math_parent(
            layout_context,
            &mut local_positioning,
            containing_block,
            style,
            stretch,
        );
        let mut denominator_output = denominator.borrow().layout_for_math_parent(
            layout_context,
            &mut local_positioning,
            containing_block,
            style,
            None,
        );

        let padding_border_inline = physical_padding.left
            + physical_padding.right
            + physical_border.left
            + physical_border.right;

        let available_inline_size =
            (containing_block.inline_size() - padding_border_inline).max(Au::zero());
        let children_width = numerator_output
            .metrics
            .size
            .inline
            .max(denominator_output.metrics.size.inline);

        let content_width = box_sizes.content_box_sizes.inline.resolve(
            LogicalAxis::Inline,
            Size::FitContent,
            Au::zero,
            Some(available_inline_size),
            || children_width.into(),
            false,
        );

        let is_display_style = style.get_font().math_style == MathStyle::Normal;
        let font_group = layout_context.font_context.font_group(style.clone_font());
        let font = font_group.first(&layout_context.font_context);
        let metrics = FractionMetrics::new(font.as_ref().map(|font| &***font), is_display_style);
        let thickness = self.resolve_line_thickness(metrics.default_thickness);

        let shifts = metrics.compute_shifts(
            thickness,
            numerator_output.metrics.ink_descent,
            denominator_output.metrics.ink_ascent,
        );

        let mut fraction_ascent = (shifts.numerator_shift_up + numerator_output.metrics.ascent())
            .max(denominator_output.metrics.ascent() - shifts.denominator_shift_down)
            .max(Au::zero());
        let mut fraction_descent = (numerator_output.metrics.descent() - shifts.numerator_shift_up)
            .max(shifts.denominator_shift_down + denominator_output.metrics.descent())
            .max(Au::zero());

        if thickness > Au::zero() {
            fraction_ascent = fraction_ascent.max(metrics.axis_height + thickness / 2);
            fraction_descent = fraction_descent.max(thickness / 2 - metrics.axis_height);
        }

        let natural_block = fraction_ascent + fraction_descent;
        let content_block = box_sizes.content_box_sizes.block.resolve(
            LogicalAxis::Block,
            Size::FitContent,
            Au::zero,
            containing_block.block_size(),
            || natural_block.into(),
            false,
        );

        let numerator_x = (content_width - numerator_output.metrics.size.inline) / 2;
        let numerator_y =
            fraction_ascent - shifts.numerator_shift_up - numerator_output.metrics.ascent();
        translate_fragments(&mut numerator_output.fragments, numerator_x, numerator_y);

        let denominator_x = (content_width - denominator_output.metrics.size.inline) / 2;
        let denominator_y =
            fraction_ascent + shifts.denominator_shift_down - denominator_output.metrics.ascent();
        translate_fragments(
            &mut denominator_output.fragments,
            denominator_x,
            denominator_y,
        );

        let bar_rect = (thickness > Au::zero()).then(|| {
            PhysicalRect::new(
                PhysicalPoint::new(
                    Au::zero(),
                    fraction_ascent - metrics.axis_height - thickness / 2,
                ),
                PhysicalSize::new(content_width, thickness),
            )
        });

        let mut children = Vec::with_capacity(
            numerator_output.fragments.len()
                + denominator_output.fragments.len()
                + self.inner_row.children.len(),
        );
        children.extend(numerator_output.fragments);
        children.extend(denominator_output.fragments);

        // Lay out any out-of-flow (position: absolute / fixed) children preserved in inner_row.
        for oof_child in &self.inner_row.children {
            let oof_output = oof_child.borrow().layout_for_math_parent(
                layout_context,
                &mut local_positioning,
                containing_block,
                style,
                None,
            );
            children.extend(oof_output.fragments);
        }

        let result = MathLayoutResult::from_content(
            self.base(),
            containing_block,
            &pbm,
            children,
            MathBoxMetrics::new(content_width, content_block, fraction_ascent),
            bar_rect.map(|bar_rect| MathPaintInfo::Fraction { bar_rect }),
        );
        self.layout_cache.store(
            self.base(),
            containing_block,
            target_size,
            &result,
            &local_positioning,
        );
        positioning_context.append(local_positioning);
        result
    }
}
