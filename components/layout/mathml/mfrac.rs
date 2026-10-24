/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::Arc;

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
use crate::formatting_contexts::Baselines;
use crate::fragment_tree::{BoxFragment, Fragment, MathFragment, MathFragmentKind};
use crate::geom::{LogicalRect, LogicalVec2, PhysicalPoint, PhysicalRect, PhysicalSize};
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{MathLayoutOutput, translate_fragments};
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult, Size, SizeConstraint};
use crate::style_ext::LayoutStyle;
use crate::{ConstraintSpace, ContainingBlock, ContainingBlockSize};

#[derive(Debug, Clone, Copy)]
pub(crate) struct FractionParameters {
    pub numerator_shift_up: Au,
    pub denominator_shift_down: Au,
}

/// Font-dependent inputs shared by fraction and stack layout.
#[derive(Debug, Clone, Copy)]
struct FractionMetrics {
    axis_height: Au,
    default_thickness: Au,
    numerator_shift_up: Au,
    denominator_shift_down: Au,
    numerator_gap_min: Au,
    denominator_gap_min: Au,
    stack_top_shift_up: Au,
    stack_bottom_shift_down: Au,
    stack_gap_min: Au,
}

impl FractionMetrics {
    fn new(font: Option<&Font>, is_display: bool) -> Self {
        if let Some(constants) = font.and_then(|font| font.math_constants(is_display)) {
            return Self {
                axis_height: constants.axis_height,
                default_thickness: constants.rule_thickness,
                numerator_shift_up: constants.num_shift_up,
                denominator_shift_down: constants.denom_shift_down,
                numerator_gap_min: constants.num_gap_min,
                denominator_gap_min: constants.denom_gap_min,
                stack_top_shift_up: constants.stack_top_shift_up,
                stack_bottom_shift_down: constants.stack_bottom_shift_down,
                stack_gap_min: constants.stack_gap_min,
            };
        }

        let (axis_height, default_thickness) = font
            .and_then(|font| font.metrics.get())
            .map(|metrics| {
                let axis_height = if metrics.x_height > Au::zero() {
                    metrics.x_height / 2
                } else {
                    metrics.ascent / 3
                };

                (axis_height, metrics.underline_size.max(Au::from_px(1)))
            })
            .unwrap_or((Au::from_px(4), Au::from_px(1)));

        Self {
            axis_height,
            default_thickness,
            numerator_shift_up: Au::zero(),
            denominator_shift_down: Au::zero(),
            numerator_gap_min: Au::zero(),
            denominator_gap_min: Au::zero(),
            stack_top_shift_up: Au::zero(),
            stack_bottom_shift_down: Au::zero(),
            stack_gap_min: (default_thickness * if is_display { 7 } else { 3 }).max(Au::from_px(7)),
        }
    }

    /// Applies the fallback rules that depend on the resolved line thickness.
    fn with_fallback_fraction_parameters(mut self, thickness: Au, is_display: bool) -> Self {
        let gap = (thickness * if is_display { 3 } else { 1 }).max(Au::from_px(2));
        let half_thickness = thickness / 2;

        self.numerator_gap_min = gap;
        self.denominator_gap_min = gap;
        self.numerator_shift_up = self.axis_height + half_thickness + gap;
        self.denominator_shift_down = half_thickness + gap;

        self
    }

    fn compute_parameters(
        &self,
        thickness: Au,
        numerator_descent: Au,
        denominator_ascent: Au,
    ) -> FractionParameters {
        if thickness > Au::zero() {
            let half_thickness = thickness / 2;
            let numerator_clearance =
                self.axis_height + half_thickness + self.numerator_gap_min + numerator_descent;
            let denominator_clearance =
                half_thickness + self.denominator_gap_min + denominator_ascent - self.axis_height;

            return FractionParameters {
                numerator_shift_up: self.numerator_shift_up.max(numerator_clearance),
                denominator_shift_down: self.denominator_shift_down.max(denominator_clearance),
            };
        }

        let mut numerator_shift_up = self.stack_top_shift_up;
        let mut denominator_shift_down = self.stack_bottom_shift_down;
        let actual_gap =
            numerator_shift_up - numerator_descent + denominator_shift_down - denominator_ascent;

        if actual_gap < self.stack_gap_min {
            let adjustment = (self.stack_gap_min - actual_gap) / 2;
            numerator_shift_up += adjustment;
            denominator_shift_down += adjustment;
        }

        FractionParameters {
            numerator_shift_up,
            denominator_shift_down,
        }
    }
}

#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathFractionBlock {
    pub inner_row: MathRowBlock,
    pub numerator: Option<ArcRefCell<MathLevelBox>>,
    pub denominator: Option<ArcRefCell<MathLevelBox>>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    pub linethickness: Option<LengthPercentage>,
    pub num_align: FractionAlignment,
    pub denom_align: FractionAlignment,
}

#[derive(Debug, MallocSizeOf, PartialEq, Eq, Clone, Copy, Default)]
pub(crate) enum FractionAlignment {
    Left,
    #[default]
    Center,
    Right,
}

impl FractionAlignment {
    fn offset(self, content_width: Au, child_width: Au) -> Au {
        match self {
            Self::Left => Au::zero(),
            Self::Center => (content_width - child_width) / 2,
            Self::Right => content_width - child_width,
        }
    }
}

/// Builds the computed-value context in one place.
///
/// The callback keeps attribute access at the call site, without requiring
/// this helper to depend on the concrete node type.
fn compute_line_thickness(
    context: &SharedStyleContext,
    style: &ServoArc<ComputedValues>,
    resolve: impl FnOnce(&Context) -> Option<LengthPercentage>,
) -> Option<LengthPercentage> {
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

    resolve(&computed_context)
}

impl MathFractionBlock {
    pub(crate) fn construct(
        context: &LayoutContext,
        node_and_style_info: &NodeAndStyleInfo,
        children: Vec<ArcRefCell<MathLevelBox>>,
    ) -> Self {
        // Keep the original children intact until validity is known, so that
        // fallback layout preserves their original order.
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
                num_align: FractionAlignment::Center,
                denom_align: FractionAlignment::Center,
            };
        }

        let element = node_and_style_info.node.as_element();
        let linethickness = compute_line_thickness(
            &context.style_context,
            &node_and_style_info.style,
            |computed_context| {
                element
                    .as_ref()
                    .and_then(|element| element.attribute(&ns!(), &local_name!("linethickness")))
                    .and_then(|attr| match attr {
                        AttrValue::LengthPercentage(_, lp) => lp.as_ref(),
                        _ => None,
                    })
                    .map(|value| value.to_computed_value(computed_context))
            },
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
            num_align: FractionAlignment::Center,
            denom_align: FractionAlignment::Center,
        }
    }

    pub(crate) fn base(&self) -> &LayoutBoxBase {
        &self.inner_row.base
    }

    pub(crate) fn base_mut(&mut self) -> &mut LayoutBoxBase {
        &mut self.inner_row.base
    }

    #[inline]
    pub(crate) fn is_valid(&self) -> bool {
        self.fraction_children().is_some()
    }

    fn fraction_children(&self) -> Option<(&ArcRefCell<MathLevelBox>, &ArcRefCell<MathLevelBox>)> {
        self.numerator.as_ref().zip(self.denominator.as_ref())
    }

    /// Percentages resolve relative to the default FractionRuleThickness.
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

        let element = node.as_element();
        self.linethickness = compute_line_thickness(context, new_style, |computed_context| {
            element
                .as_ref()
                .and_then(|element| element.attribute(&ns!(), &local_name!("linethickness")))
                .and_then(AttrValue::as_length_percentage)
                .map(|value| value.to_computed_value(computed_context))
        });
    }

    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        self.inner_row.attached_to_tree(layout_box);
    }

    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
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

        InlineContentSizesResult {
            sizes: ContentSizes {
                min_content: numerator_sizes
                    .sizes
                    .min_content
                    .max(denominator_sizes.sizes.min_content),
                max_content: numerator_sizes
                    .sizes
                    .max_content
                    .max(denominator_sizes.sizes.max_content),
            },
            depends_on_block_constraints: numerator_sizes.depends_on_block_constraints
                || denominator_sizes.depends_on_block_constraints,
        }
    }

    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<(Au, Au)>,
    ) -> MathLayoutOutput {
        let Some((numerator, denominator)) = self.fraction_children() else {
            return self.inner_row.layout(
                layout_context,
                positioning_context,
                containing_block,
                None,
            );
        };

        let style = &self.base().style;
        let writing_mode = style.writing_mode;
        let layout_style = LayoutStyle::Default(style);
        let box_sizes =
            layout_style.content_box_sizes_and_padding_border_margin(&containing_block.into());
        let pbm = box_sizes.pbm;
        let physical_padding = pbm.padding.to_physical(writing_mode);
        let physical_border = pbm.border.to_physical(writing_mode);

        let constraint_space = ConstraintSpace::new(SizeConstraint::default(), style, None);

        let numerator_sizes = numerator
            .borrow()
            .inline_content_sizes(layout_context, &constraint_space);
        let denominator_sizes = denominator
            .borrow()
            .inline_content_sizes(layout_context, &constraint_space);

        let numerator_cb = ContainingBlock {
            size: ContainingBlockSize {
                inline: numerator_sizes.sizes.max_content,
                block: containing_block.size.block,
            },
            style: containing_block.style,
        };
        let denominator_cb = ContainingBlock {
            size: ContainingBlockSize {
                inline: denominator_sizes.sizes.max_content,
                block: containing_block.size.block,
            },
            style: containing_block.style,
        };

        // Only the numerator receives the stretch target.
        let mut numerator_output = numerator.borrow().layout(
            layout_context,
            positioning_context,
            &numerator_cb,
            target_size,
        );
        let mut denominator_output =
            denominator
                .borrow()
                .layout(layout_context, positioning_context, &denominator_cb, None);

        let padding_border_inline = physical_padding.left
            + physical_padding.right
            + physical_border.left
            + physical_border.right;
        let padding_border_block = physical_padding.top
            + physical_padding.bottom
            + physical_border.top
            + physical_border.bottom;

        let available_inline_size =
            (containing_block.size.inline - padding_border_inline).max(Au::zero());
        let children_width = numerator_output
            .inline_size
            .max(denominator_output.inline_size);

        let content_width = box_sizes.content_box_sizes.inline.resolve(
            LogicalAxis::Inline,
            Size::FitContent,
            || containing_block.size.inline,
            Some(available_inline_size),
            || children_width.into(),
            false,
        );

        let is_display = style.get_font().math_style == MathStyle::Normal;
        let font_group = layout_context.font_context.font_group(style.clone_font());
        let font = font_group.first(&layout_context.font_context);
        let font_ref = font.as_ref().map(|font| &***font);

        // Obtain MATH constants once and use the same snapshot throughout.
        let math_constants = font_ref.and_then(|font| font.math_constants(is_display));
        let has_math_constants = math_constants.is_some();

        let metrics = if let Some(constants) = math_constants {
            FractionMetrics {
                axis_height: constants.axis_height,
                default_thickness: constants.rule_thickness,
                numerator_shift_up: constants.num_shift_up,
                denominator_shift_down: constants.denom_shift_down,
                numerator_gap_min: constants.num_gap_min,
                denominator_gap_min: constants.denom_gap_min,
                stack_top_shift_up: constants.stack_top_shift_up,
                stack_bottom_shift_down: constants.stack_bottom_shift_down,
                stack_gap_min: constants.stack_gap_min,
            }
        } else {
            FractionMetrics::new(None, is_display)
        };

        // Use ordinary font metrics when the font has no MATH table.
        let metrics = if has_math_constants {
            metrics
        } else {
            let (axis_height, default_thickness) = font_ref
                .and_then(|font| font.metrics.get())
                .map(|metrics| {
                    let axis_height = if metrics.x_height > Au::zero() {
                        metrics.x_height / 2
                    } else {
                        metrics.ascent / 3
                    };

                    (axis_height, metrics.underline_size.max(Au::from_px(1)))
                })
                .unwrap_or((Au::from_px(4), Au::from_px(1)));

            FractionMetrics {
                axis_height,
                default_thickness,
                stack_gap_min: (default_thickness * if is_display { 7 } else { 3 })
                    .max(Au::from_px(7)),
                ..metrics
            }
        };

        let thickness = self.resolve_line_thickness(metrics.default_thickness);
        let metrics = if has_math_constants {
            metrics
        } else {
            metrics.with_fallback_fraction_parameters(thickness, is_display)
        };

        let parameters = metrics.compute_parameters(
            thickness,
            numerator_output.descent,
            denominator_output.ascent,
        );

        let mut fraction_ascent = parameters.numerator_shift_up + numerator_output.ascent;
        let mut fraction_descent = parameters.denominator_shift_down + denominator_output.descent;

        if thickness > Au::zero() {
            fraction_ascent = fraction_ascent.max(metrics.axis_height + thickness / 2);
            fraction_descent = fraction_descent.max(thickness / 2 - metrics.axis_height);
        }

        let fraction_ascent = fraction_ascent.max(Au::zero());
        let fraction_descent = fraction_descent.max(Au::zero());
        let content_block = fraction_ascent + fraction_descent;

        let numerator_x = self
            .num_align
            .offset(content_width, numerator_output.inline_size);
        let numerator_y = fraction_ascent - parameters.numerator_shift_up - numerator_output.ascent;
        translate_fragments(&mut numerator_output.fragments, numerator_x, numerator_y);

        let denominator_x = self
            .denom_align
            .offset(content_width, denominator_output.inline_size);
        let denominator_y =
            fraction_ascent + parameters.denominator_shift_down - denominator_output.ascent;
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
            numerator_output.fragments.len() + denominator_output.fragments.len(),
        );
        children.extend(numerator_output.fragments);
        children.extend(denominator_output.fragments);

        let content_rect = LogicalRect {
            start_corner: LogicalVec2 {
                inline: pbm.padding.inline_start + pbm.border.inline_start,
                block: pbm.padding.block_start + pbm.border.block_start,
            },
            size: LogicalVec2 {
                inline: content_width,
                block: content_block,
            },
        }
        .as_physical(Some(containing_block));

        let fraction_baseline = physical_padding.top + physical_border.top + fraction_ascent;
        let baselines = Baselines {
            first: Some(fraction_baseline),
            last: Some(fraction_baseline),
        };

        let box_fragment = BoxFragment::new(
            self.base().base_fragment_info.clone(),
            style.clone(),
            children,
            content_rect,
            physical_padding,
            physical_border,
            pbm.margin.auto_is(Au::zero).to_physical(writing_mode),
            None,
        )
        .with_baselines(baselines);

        MathLayoutOutput {
            fragments: vec![Fragment::Math(Arc::new(MathFragment {
                box_fragment: box_fragment.into(),
                kind: MathFragmentKind::Fraction { bar_rect },
            }))],
            inline_size: content_width + padding_border_inline,
            block_size: content_block + padding_border_block,
            ascent: fraction_baseline,
            descent: fraction_descent + physical_padding.bottom + physical_border.bottom,
        }
    }
}
