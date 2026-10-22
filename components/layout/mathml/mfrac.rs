use app_units::Au;
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::computed_values::position::T;
use style::logical_geometry::LogicalAxis;
use style::values::generics::position::Position;
use std::sync::Arc;
use style::context::SharedStyleContext;
use style::properties::ComputedValues;

use super::{MathLevelBox, MathRowBlock};
use crate::cell::ArcRefCell;
use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::dom_traversal::NodeAndStyleInfo;
use crate::formatting_contexts::Baselines;
use crate::fragment_tree::{
    BaseFragmentInfo, BoxFragment, Fragment, MathFragment, MathFragmentKind,
};
use crate::geom::{
    LogicalRect, LogicalVec2, PhysicalPoint, PhysicalRect, PhysicalSides, PhysicalSize, PhysicalVec,
};
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{MathLayoutOutput, math_axis_height, translate_fragments};
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult, Size, SizeConstraint};
use crate::style_ext::LayoutStyle;
use crate::{ConstraintSpace, ContainingBlock, ContainingBlockSize};
use style::Zero;

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
    pub linethickness: Option<Au>,
    pub num_align: FractionAlignment,
    pub denom_align: FractionAlignment,
}
#[derive(Debug, MallocSizeOf, PartialEq, Eq, Clone, Copy)]
pub(crate) enum FractionAlignment {
    Left,
    Center,
    Right,
}
impl MathFractionBlock {
    pub(crate) fn construct(
        context: &LayoutContext,
        node_and_style_info: &NodeAndStyleInfo,
        children: Vec<ArcRefCell<MathLevelBox>>,
    ) -> Self {
        // Only in-flow children count as numerator / denominator
        let mut in_flow_children: Vec<ArcRefCell<MathLevelBox>> = Vec::new();
        let mut out_of_flow: Vec<ArcRefCell<MathLevelBox>> = Vec::new();
        for child in children {
            let is_in_flow = child.borrow().with_base(|b| {
                !b.style.get_box().position.is_absolutely_positioned()
            });
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
                linethickness: None,
                num_align: FractionAlignment::Center,
                denom_align: FractionAlignment::Center,
            }
        } else {
            // Fallback: lay out all children as an mrow
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
    pub(crate) fn is_valid(&self) -> bool {
        self.numerator.is_some() && self.denominator.is_some()
    }
    pub(crate) fn default_line_thickness(&self) -> Au {
        Au::from_px(1)
    }
    pub(crate) fn line_thickness(&self) -> Au {
        self.linethickness
            .unwrap_or_else(|| self.default_line_thickness())
    }
    fn fraction_parameters(
        &self,
        thickness: Au,
        math_axis_height: Au,
        num_descent: Au,
        denom_ascent: Au,
    ) -> FractionParameters {
        let num_gap_min = (thickness * 2).max(Au::from_px(2));
        let denom_gap_min = (thickness * 2).max(Au::from_px(2));
        let num_shift_up = math_axis_height + thickness / 2 + num_gap_min + num_descent;
        let denom_shift_down = thickness / 2 + denom_gap_min + denom_ascent - math_axis_height;
        FractionParameters {
            numerator_shift_up: num_shift_up.max(math_axis_height + thickness / 2 + num_gap_min),
            denominator_shift_down: denom_shift_down.max(thickness / 2 + denom_gap_min),
        }
    }
    fn stack_parameters(&self, num_descent: Au, denom_ascent: Au) -> FractionParameters {
        let gap_min = Au::from_px(7);
        let mut num_shift_up = Au::zero();
        let mut denom_shift_down = Au::zero();
        let gap = num_shift_up - num_descent + denom_shift_down - denom_ascent;
        if gap < gap_min {
            let delta = (gap_min - gap) / 2;
            num_shift_up += delta;
            denom_shift_down += delta;
        }
        FractionParameters {
            numerator_shift_up: num_shift_up,
            denominator_shift_down: denom_shift_down,
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
    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        _target_size: Option<(Au, Au)>,
    ) -> MathLayoutOutput {
        if !self.is_valid() {
            return self
                .inner_row
                .layout(layout_context, positioning_context, containing_block, None);
        }
        let style = &self.base().style;
        let writing_mode = style.writing_mode;
        let layout_style = LayoutStyle::Default(style);
        let content_box_sizes_and_pbm = layout_style
            .content_box_sizes_and_padding_border_margin(&containing_block.into());
        let pbm = content_box_sizes_and_pbm.pbm;
        let physical_padding = pbm.padding.to_physical(writing_mode);
        let physical_border = pbm.border.to_physical(writing_mode);
        let num_box = self.numerator.as_ref().unwrap();
        let denom_box = self.denominator.as_ref().unwrap();
        let unconstrained_cb = ContainingBlock {
            size: ContainingBlockSize {
                inline: containing_block.size.inline,
                block: containing_block.size.block,
            },
            style: containing_block.style,
        };
        let mut num_out = num_box
            .borrow()
            .layout(layout_context, positioning_context, &unconstrained_cb, _target_size);
        let mut denom_out = denom_box
            .borrow()
            .layout(layout_context, positioning_context, &unconstrained_cb, None);
        let padding_border_inline = physical_padding.left + physical_padding.right
            + physical_border.left + physical_border.right;
        let available_inline_size =
            Au::zero().max(containing_block.size.inline - padding_border_inline);
        // Content inline size: resolve CSS width, min-width, and max-width against children_width
        let children_width = num_out.inline_size.max(denom_out.inline_size);
        let content_width = content_box_sizes_and_pbm
            .content_box_sizes
            .inline
            .resolve(
                LogicalAxis::Inline,
                Size::FitContent,
                || containing_block.size.inline,
                Some(available_inline_size),
                || children_width.into(),
                false,
            );
        let thickness = self.line_thickness();
        let font_group = layout_context.font_context.font_group(style.clone_font());
        let font = font_group.first(&layout_context.font_context);
        let math_axis_height = font.as_ref().map(math_axis_height).unwrap_or(Au::from_px(4));
        let params = if thickness > Au::zero() {
            self.fraction_parameters(thickness, math_axis_height, num_out.descent, denom_out.ascent)
        } else {
            self.stack_parameters(num_out.descent, denom_out.ascent)
        };
        // Typographic bounds relative to the fraction baseline
        let fraction_ascent = params.numerator_shift_up + num_out.ascent;
        let fraction_descent = params.denominator_shift_down + denom_out.descent;
        let content_block = fraction_ascent + fraction_descent;
        // Content box start corner in physical coordinates
        let content_left = physical_padding.left + physical_border.left;
        let content_top = physical_padding.top + physical_border.top;
        // Position numerator
        let num_x = content_left
            + self.horizontal_offset(content_width, num_out.inline_size, self.num_align);
        let num_y = content_top;
        translate_fragments(&mut num_out.fragments, num_x, num_y);
        // Position denominator below fraction bar
        let denom_x = content_left
            + self.horizontal_offset(content_width, denom_out.inline_size, self.denom_align);
        let denom_y = content_top + fraction_ascent + params.denominator_shift_down - denom_out.ascent;
        translate_fragments(&mut denom_out.fragments, denom_x, denom_y);
        // Position fraction bar centered at math axis (y = fraction_ascent - math_axis_height)
        let bar_center_y = fraction_ascent - math_axis_height;
        let bar_y = bar_center_y - (thickness / 2);
        let bar_rect = if thickness > Au::zero() {
            Some(PhysicalRect::new(
                PhysicalPoint::new(content_left, content_top + bar_y),
                PhysicalSize::new(content_width, thickness),
            ))
        } else {
            None
        };
        let mut children = Vec::with_capacity(num_out.fragments.len() + denom_out.fragments.len());
        children.extend(num_out.fragments);
        children.extend(denom_out.fragments);
        let padding_border_inline = physical_padding.left + physical_padding.right
            + physical_border.left + physical_border.right;
        let padding_border_block = physical_padding.top + physical_padding.bottom
            + physical_border.top + physical_border.bottom;
        let total_inline = content_width + padding_border_inline;
        let total_block = content_block + padding_border_block;
        // Content box rect relative to the box origin
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
        // Baseline: distance from box top to baseline
        let fraction_baseline = content_top + fraction_ascent;
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