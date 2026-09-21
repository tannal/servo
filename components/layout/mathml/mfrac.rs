use app_units::Au;
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use std::sync::Arc;
use style::context::SharedStyleContext;
use style::properties::ComputedValues;
use style::values::computed::Context;

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
use crate::sizing::{ContentSizes, InlineContentSizesResult, SizeConstraint};
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
        _context: &LayoutContext,
        node_and_style_info: &NodeAndStyleInfo,
        mut children: Vec<ArcRefCell<MathLevelBox>>,
    ) -> Self {
        let num_align = FractionAlignment::Center;
        let denom_align = FractionAlignment::Center;
        let numerator = if !children.is_empty() {
            Some(children.remove(0))
        } else {
            None
        };
        let denominator = if !children.is_empty() {
            Some(children.remove(0))
        } else {
            None
        };
        let inner_row = MathRowBlock::construct(node_and_style_info, Vec::new());
        MathFractionBlock {
            inner_row,
            numerator,
            denominator,
            linethickness: None,
            num_align,
            denom_align,
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
        let pbm = layout_style
            .content_box_sizes_and_padding_border_margin(&containing_block.into())
            .pbm;
        let physical_padding = pbm.padding.to_physical(writing_mode);
        let num_box = self.numerator.as_ref().unwrap();
        let denom_box = self.denominator.as_ref().unwrap();
        // 1. Measure intrinsic inline sizes
        let constraint_space = ConstraintSpace::new(SizeConstraint::default(), style, None);
        let num_inline_width = num_box
            .borrow()
            .inline_content_sizes(layout_context, &constraint_space)
            .sizes
            .max_content;
        let denom_inline_width = denom_box
            .borrow()
            .inline_content_sizes(layout_context, &constraint_space)
            .sizes
            .max_content;
        let num_containing_block = ContainingBlock {
            size: ContainingBlockSize {
                inline: num_inline_width,
                block: containing_block.size.block,
            },
            style: containing_block.style,
        };
        let denom_containing_block = ContainingBlock {
            size: ContainingBlockSize {
                inline: denom_inline_width,
                block: containing_block.size.block,
            },
            style: containing_block.style,
        };
        // 2. Layout children in O(1) without measure()
        let mut num_out = num_box
            .borrow()
            .layout(layout_context, positioning_context, &num_containing_block, None);
        let mut denom_out = denom_box
            .borrow()
            .layout(layout_context, positioning_context, &denom_containing_block, None);
        let content_width = num_out.inline_size.max(denom_out.inline_size);
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
        // Position numerator at top (y = 0 in content box)
        let num_x = physical_padding.left
            + self.horizontal_offset(content_width, num_out.inline_size, self.num_align);
        let num_y = physical_padding.top;
        translate_fragments(&mut num_out.fragments, num_x, num_y);
        // Position denominator below fraction bar
        let denom_x = physical_padding.left
            + self.horizontal_offset(content_width, denom_out.inline_size, self.denom_align);
        let denom_y = physical_padding.top + fraction_ascent + params.denominator_shift_down - denom_out.ascent;
        translate_fragments(&mut denom_out.fragments, denom_x, denom_y);
        // Position fraction bar centered at math axis (y = fraction_ascent - math_axis_height)
        let bar_center_y = fraction_ascent - math_axis_height;
        let bar_y = bar_center_y - (thickness / 2);
        let bar_rect = if thickness > Au::zero() {
            Some(PhysicalRect::new(
                PhysicalPoint::new(physical_padding.left, physical_padding.top + bar_y),
                PhysicalSize::new(content_width, thickness),
            ))
        } else {
            None
        };
        let mut children = Vec::with_capacity(num_out.fragments.len() + denom_out.fragments.len());
        children.extend(num_out.fragments);
        children.extend(denom_out.fragments);
        let total_block = physical_padding.top
            + fraction_ascent
            + fraction_descent
            + physical_padding.bottom;
        let total_inline = content_width + physical_padding.left + physical_padding.right;
        let content_size = LogicalVec2 {
            inline: total_inline,
            block: total_block,
        };
        let content_rect = LogicalRect {
            start_corner: LogicalVec2::zero(),
            size: content_size,
        }
        .as_physical(Some(containing_block));
        // Baseline: distance from box top to baseline
        let fraction_baseline = physical_padding.top + fraction_ascent;
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
            pbm.border.to_physical(writing_mode),
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
            descent: fraction_descent + physical_padding.bottom,
        }
    }
}