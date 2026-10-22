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

impl MathFractionBlock {
    pub(crate) fn construct(
        context: &LayoutContext,
        node_and_style_info: &NodeAndStyleInfo,
        children: Vec<ArcRefCell<MathLevelBox>>,
    ) -> Self {
        let element = node_and_style_info.node.as_element();

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

        let linethickness = element
            .as_ref()
            .and_then(|el| el.attribute(&ns!(), &local_name!("linethickness")))
            .and_then(AttrValue::as_length_percentage)
            .map(|lp| lp.to_computed_value(&to_computed_context));

        // Only in-flow children count as numerator / denominator
        let mut in_flow_children: Vec<ArcRefCell<MathLevelBox>> =
            Vec::with_capacity(children.len());
        let mut out_of_flow: Vec<ArcRefCell<MathLevelBox>> = Vec::new();
        for child in children {
            let is_in_flow = child
                .borrow()
                .with_base(|b| !b.style.get_box().position.is_absolutely_positioned());
            if is_in_flow {
                in_flow_children.push(child);
            } else {
                out_of_flow.push(child);
            }
        }

        if in_flow_children.len() == 2 {
            let denominator = Some(in_flow_children.pop().unwrap());
            let numerator = Some(in_flow_children.pop().unwrap());
            let inner_row = MathRowBlock::construct(node_and_style_info, out_of_flow);
            MathFractionBlock {
                inner_row,
                numerator,
                denominator,
                linethickness,
                num_align: FractionAlignment::Center,
                denom_align: FractionAlignment::Center,
            }
        } else {
            // Fallback: lay out all children as an mrow (MathML Core §3.3.2)
            let mut all_children = in_flow_children;
            all_children.extend(out_of_flow);
            let inner_row = MathRowBlock::construct(node_and_style_info, all_children);
            MathFractionBlock {
                inner_row,
                numerator: None,
                denominator: None,
                linethickness: None,
                num_align: FractionAlignment::Center,
                denom_align: FractionAlignment::Center,
            }
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
        self.numerator.is_some() && self.denominator.is_some()
    }

    /// Resolves the actual line thickness using the default FractionRuleThickness from font metrics.
    /// Per MathML Core §3.2.2, percentages resolve relative to default_thickness.
    pub(crate) fn resolve_line_thickness(&self, default_thickness: Au) -> Au {
        match self.linethickness {
            Some(ref lp) => lp.to_used_value(default_thickness).max(Au::zero()),
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
    }

    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        self.inner_row.attached_to_tree(layout_box);
    }

    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        if !self.is_valid() {
            return self
                .inner_row
                .inline_content_sizes(layout_context, constraint_space);
        }
        let zero_sizes = InlineContentSizesResult {
            sizes: ContentSizes {
                min_content: Au::zero(),
                max_content: Au::zero(),
            },
            depends_on_block_constraints: false,
        };
        let num_sizes = self.numerator.as_ref().map_or(zero_sizes.clone(), |num| {
            num.borrow()
                .inline_content_sizes(layout_context, constraint_space)
        });
        let denom_sizes = self.denominator.as_ref().map_or(zero_sizes, |denom| {
            denom
                .borrow()
                .inline_content_sizes(layout_context, constraint_space)
        });

        let min_content = num_sizes
            .sizes
            .min_content
            .max(denom_sizes.sizes.min_content);
        let max_content = num_sizes
            .sizes
            .max_content
            .max(denom_sizes.sizes.max_content);

        InlineContentSizesResult {
            sizes: ContentSizes {
                min_content,
                max_content,
            },
            depends_on_block_constraints: num_sizes.depends_on_block_constraints
                || denom_sizes.depends_on_block_constraints,
        }
    }

    #[inline]
    fn horizontal_offset(
        &self,
        content_width: Au,
        child_width: Au,
        align: FractionAlignment,
    ) -> Au {
        match align {
            FractionAlignment::Left => Au::zero(),
            FractionAlignment::Center => (content_width - child_width) / 2,
            FractionAlignment::Right => content_width - child_width,
        }
    }

    /// Computes fraction layout parameters according to OpenType MATH metrics or MathML Core §5 fallback.
    fn compute_parameters(
        &self,
        font: Option<&Font>,
        is_display: bool,
        thickness: Au,
        num_descent: Au,
        denom_ascent: Au,
    ) -> (FractionParameters, Au, Au) {
        let math_constants = font.and_then(|f| f.math_constants(is_display));

        if let Some(c) = math_constants {
            let axis_height = c.axis_height;
            let default_thickness = c.rule_thickness;

            if thickness > Au::zero() {
                let half_thickness = thickness / 2;
                let num_clearance = axis_height + half_thickness + c.num_gap_min + num_descent;
                let denom_clearance = half_thickness + c.denom_gap_min + denom_ascent - axis_height;
                (
                    FractionParameters {
                        numerator_shift_up: c.num_shift_up.max(num_clearance),
                        denominator_shift_down: c.denom_shift_down.max(denom_clearance),
                    },
                    axis_height,
                    default_thickness,
                )
            } else {
                let mut num_shift_up = c.stack_top_shift_up;
                let mut denom_shift_down = c.stack_bottom_shift_down;
                let actual_gap = (num_shift_up - num_descent) + (denom_shift_down - denom_ascent);
                if actual_gap < c.stack_gap_min {
                    let delta = (c.stack_gap_min - actual_gap) / 2;
                    num_shift_up += delta;
                    denom_shift_down += delta;
                }
                (
                    FractionParameters {
                        numerator_shift_up: num_shift_up,
                        denominator_shift_down: denom_shift_down,
                    },
                    axis_height,
                    default_thickness,
                )
            }
        } else {
            // MathML Core §5 Fallback implementation
            let font_metrics = font.and_then(|f| f.metrics.get());
            let (axis_height, default_thickness) = if let Some(metrics) = font_metrics {
                let axis = if metrics.x_height > Au::zero() {
                    metrics.x_height / 2
                } else {
                    metrics.ascent / 3
                };
                let rule = metrics.underline_size.max(Au::from_px(1));
                (axis, rule)
            } else {
                (Au::from_px(4), Au::from_px(1))
            };

            if thickness > Au::zero() {
                let gap_factor = if is_display { 3 } else { 1 };
                let num_gap_min = (thickness * gap_factor).max(Au::from_px(2));
                let denom_gap_min = (thickness * gap_factor).max(Au::from_px(2));
                let half_thickness = thickness / 2;
                let num_shift_up = axis_height + half_thickness + num_gap_min + num_descent;
                let denom_shift_down = half_thickness + denom_gap_min + denom_ascent - axis_height;
                (
                    FractionParameters {
                        numerator_shift_up: num_shift_up
                            .max(axis_height + half_thickness + num_gap_min),
                        denominator_shift_down: denom_shift_down
                            .max(half_thickness + denom_gap_min),
                    },
                    axis_height,
                    default_thickness,
                )
            } else {
                let gap_min = if is_display {
                    default_thickness * 7
                } else {
                    default_thickness * 3
                }
                .max(Au::from_px(7));
                let mut num_shift_up = Au::zero();
                let mut denom_shift_down = Au::zero();
                let actual_gap = (num_shift_up - num_descent) + (denom_shift_down - denom_ascent);
                if actual_gap < gap_min {
                    let delta = (gap_min - actual_gap) / 2;
                    num_shift_up += delta;
                    denom_shift_down += delta;
                }
                (
                    FractionParameters {
                        numerator_shift_up: num_shift_up,
                        denominator_shift_down: denom_shift_down,
                    },
                    axis_height,
                    default_thickness,
                )
            }
        }
    }

    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<(Au, Au)>,
    ) -> MathLayoutOutput {
        if !self.is_valid() {
            return self.inner_row.layout(
                layout_context,
                positioning_context,
                containing_block,
                None,
            );
        }

        let style = &self.base().style;
        let writing_mode = style.writing_mode;
        let layout_style = LayoutStyle::Default(style);
        let content_box_sizes_and_pbm =
            layout_style.content_box_sizes_and_padding_border_margin(&containing_block.into());
        let pbm = content_box_sizes_and_pbm.pbm;
        let physical_padding = pbm.padding.to_physical(writing_mode);
        let physical_border = pbm.border.to_physical(writing_mode);

        let num_box = self.numerator.as_ref().unwrap();
        let denom_box = self.denominator.as_ref().unwrap();
        let constraint_space = ConstraintSpace::new(SizeConstraint::default(), style, None);
        let num_sizes = num_box
            .borrow()
            .inline_content_sizes(layout_context, &constraint_space);
        let denom_sizes = denom_box
            .borrow()
            .inline_content_sizes(layout_context, &constraint_space);
        let num_cb = ContainingBlock {
            size: ContainingBlockSize {
                inline: num_sizes.sizes.max_content,
                block: containing_block.size.block,
            },
            style: containing_block.style,
        };
        let denom_cb = ContainingBlock {
            size: ContainingBlockSize {
                inline: denom_sizes.sizes.max_content,
                block: containing_block.size.block,
            },
            style: containing_block.style,
        };
        // Layout numerator with target_size, denominator unconstrained (MathML Core §3.3.2.1)
        let mut num_out = num_box.borrow().layout(
            layout_context,
            positioning_context,
            &num_cb,
            target_size,
        );
        let mut denom_out = denom_box.borrow().layout(
            layout_context,
            positioning_context,
            &denom_cb,
            None,
        );

        let padding_border_inline = physical_padding.left
            + physical_padding.right
            + physical_border.left
            + physical_border.right;
        let available_inline_size =
            Au::zero().max(containing_block.size.inline - padding_border_inline);

        let children_width = num_out.inline_size.max(denom_out.inline_size);
        let content_width = content_box_sizes_and_pbm.content_box_sizes.inline.resolve(
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

        let initial_default_thickness = font
            .as_ref()
            .and_then(|f| f.math_constants(is_display))
            .map(|c| c.rule_thickness)
            .unwrap_or_else(|| {
                font.as_ref()
                    .and_then(|f| f.metrics.get())
                    .map(|m| m.underline_size.max(Au::from_px(1)))
                    .unwrap_or(Au::from_px(1))
            });

        let thickness = self.resolve_line_thickness(initial_default_thickness);

        let (params, math_axis_height, _) = self.compute_parameters(
            font.as_ref().map(|f| &**f).map(|v| &**v),
            is_display,
            thickness,
            num_out.descent,
            denom_out.ascent,
        );

        // Typographic bounds relative to the fraction baseline (MathML Core §3.3.2)
        let fraction_ascent = if thickness > Au::zero() {
            (params.numerator_shift_up + num_out.ascent).max(math_axis_height + thickness / 2)
        } else {
            params.numerator_shift_up + num_out.ascent
        }
        .max(Au::zero());

        let fraction_descent = if thickness > Au::zero() {
            (params.denominator_shift_down + denom_out.descent)
                .max(thickness / 2 - math_axis_height)
        } else {
            params.denominator_shift_down + denom_out.descent
        }
        .max(Au::zero());

        let content_block = fraction_ascent + fraction_descent;

        // Position numerator relative to math content box (top-left at (0, 0))
        let num_x = self.horizontal_offset(content_width, num_out.inline_size, self.num_align);
        let num_y = fraction_ascent - params.numerator_shift_up - num_out.ascent;
        translate_fragments(&mut num_out.fragments, num_x, num_y);

        // Position denominator relative to math content box (top-left at (0, 0))
        let denom_x =
            self.horizontal_offset(content_width, denom_out.inline_size, self.denom_align);
        let denom_y = fraction_ascent + params.denominator_shift_down - denom_out.ascent;
        translate_fragments(&mut denom_out.fragments, denom_x, denom_y);

        // Fraction bar centered at math axis relative to math content box
        let bar_rect = if thickness > Au::zero() {
            let bar_y = fraction_ascent - math_axis_height - (thickness / 2);
            Some(PhysicalRect::new(
                PhysicalPoint::new(Au::zero(), bar_y),
                PhysicalSize::new(content_width, thickness),
            ))
        } else {
            None
        };

        let mut children = Vec::with_capacity(num_out.fragments.len() + denom_out.fragments.len());
        children.extend(num_out.fragments);
        children.extend(denom_out.fragments);

        let padding_border_block = physical_padding.top
            + physical_padding.bottom
            + physical_border.top
            + physical_border.bottom;
        let total_inline = content_width + padding_border_inline;
        let total_block = content_block + padding_border_block;

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
            self.base().style.clone(),
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
            inline_size: total_inline,
            block_size: total_block,
            ascent: fraction_baseline,
            descent: fraction_descent + physical_padding.bottom + physical_border.bottom,
        }
    }
}
