use app_units::Au;
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use std::sync::Arc;
use style::context::SharedStyleContext;
use style::properties::ComputedValues;

use super::MathLevelBox;
use crate::cell::ArcRefCell;
use crate::context::LayoutContext;
use crate::dom::{BoxSlot, LayoutBox, WeakLayoutBox};
use crate::dom_traversal::{BoxTreeString, Contents, NodeAndStyleInfo, TraversalHandler};
use crate::formatting_contexts::Baselines;
use crate::fragment_tree::{
    BaseFragmentInfo, BoxFragment, Fragment, MathFragment, MathFragmentKind,
};
use crate::geom::{LogicalRect, LogicalVec2, PhysicalVec, ToLogicalWithContainingBlock};
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{MathLayoutOutput, MathTraversalBuilder, math_axis_height, translate_fragments};
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult, SizeConstraint};
use crate::style_ext::{DisplayGeneratingBox, LayoutStyle};
use crate::{ConstraintSpace, ContainingBlock, ContainingBlockSize, PropagatedBoxTreeData};
use style::Zero;

#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathRowBlock {
    pub base: LayoutBoxBase,
    pub children: Vec<ArcRefCell<MathLevelBox>>,
}
impl MathRowBlock {
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
        MathRowBlock { base, children }
    }
    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
        for child in &self.children {
            child.borrow_mut().repair_style(context, node, new_style);
        }
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
        let mut min_content = Au::zero();
        let mut max_content = Au::zero();
        let mut depends_on_block_constraints = false;
        for child in &self.children {
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
    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        _target_size: Option<(Au, Au)>,
    ) -> MathLayoutOutput {
        if self.children.is_empty() {
            return MathLayoutOutput::empty();
        }
        let style = &self.base.style;
        let writing_mode = style.writing_mode;
        let layout_style = LayoutStyle::Default(style);
        let pbm = layout_style
            .content_box_sizes_and_padding_border_margin(&containing_block.into())
            .pbm;
        let physical_padding = pbm.padding.to_physical(writing_mode);
        let mut row_max_ascent = Au::zero();
        let mut row_max_descent = Au::zero();
        // ------------------------------------------------------------------
        // PASS 1: Layout non-stretchy elements in O(1)
        // ------------------------------------------------------------------
        let mut outputs: Vec<Option<MathLayoutOutput>> = Vec::with_capacity(self.children.len());
        for child_cell in &self.children {
            let child = child_cell.borrow();
            if !child.is_stretchy_operator() {
                let constraint_space = ConstraintSpace::new(SizeConstraint::default(), style, None);
                let content_sizes = child.inline_content_sizes(layout_context, &constraint_space);
                let child_containing_block = ContainingBlock {
                    size: ContainingBlockSize {
                        inline: content_sizes.sizes.max_content,
                        block: containing_block.size.block,
                    },
                    style: containing_block.style,
                };
                let out = child.layout(
                    layout_context,
                    positioning_context,
                    &child_containing_block,
                    None,
                );
                row_max_ascent = row_max_ascent.max(out.ascent);
                row_max_descent = row_max_descent.max(out.descent);
                outputs.push(Some(out));
            } else {
                outputs.push(None);
            }
        }
        // Fallback for row with only stretchy operators
        if row_max_ascent == Au::zero() && row_max_descent == Au::zero() {
            row_max_ascent = Au::from_px(15);
            row_max_descent = Au::from_px(5);
        }
        // Compute symmetric stretch target centered on math axis
        let font_group = layout_context.font_context.font_group(style.clone_font());
        let font = font_group.first(&layout_context.font_context);
        let axis_height = font.as_ref().map(math_axis_height).unwrap_or(Au::from_px(4));
        let dist_above = (row_max_ascent - axis_height).max(Au::zero());
        let dist_below = (row_max_descent + axis_height).max(Au::zero());
        let half_height = dist_above.max(dist_below);
        let target_ascent = axis_height + half_height;
        let target_descent = (half_height - axis_height).max(Au::zero());
        // ------------------------------------------------------------------
        // PASS 2: Layout stretchy operators with symmetric target size
        // ------------------------------------------------------------------
        for (i, child_cell) in self.children.iter().enumerate() {
            if outputs[i].is_none() {
                let child = child_cell.borrow();
                let constraint_space = ConstraintSpace::new(SizeConstraint::default(), style, None);
                let content_sizes = child.inline_content_sizes(layout_context, &constraint_space);
                let child_containing_block = ContainingBlock {
                    size: ContainingBlockSize {
                        inline: content_sizes.sizes.max_content,
                        block: containing_block.size.block,
                    },
                    style: containing_block.style,
                };
                let out = child.layout(
                    layout_context,
                    positioning_context,
                    &child_containing_block,
                    Some((target_ascent, target_descent)),
                );
                row_max_ascent = row_max_ascent.max(out.ascent);
                row_max_descent = row_max_descent.max(out.descent);
                outputs[i] = Some(out);
            }
        }
        // ------------------------------------------------------------------
        // PASS 3: Align all items along the common baseline
        // ------------------------------------------------------------------
        let mut child_fragments = Vec::new();
        let mut current_x = physical_padding.left;
        for mut out in outputs.into_iter().flatten() {
            let y_offset = physical_padding.top + (row_max_ascent - out.ascent);
            translate_fragments(&mut out.fragments, current_x, y_offset);
            current_x += out.inline_size;
            child_fragments.extend(out.fragments);
        }
        let total_inline = current_x + physical_padding.right;
        let total_block = physical_padding.top
            + row_max_ascent
            + row_max_descent
            + physical_padding.bottom;
        let content_size = LogicalVec2 {
            inline: total_inline,
            block: total_block,
        };
        let content_rect = LogicalRect {
            start_corner: LogicalVec2::zero(),
            size: content_size,
        }
        .as_physical(Some(containing_block));
        let row_baseline = physical_padding.top + row_max_ascent;
        let baselines = Baselines {
            first: Some(row_baseline),
            last: Some(row_baseline),
        };
        let box_fragment = BoxFragment::new(
            self.base.base_fragment_info.clone(),
            self.base.style.clone(),
            child_fragments,
            content_rect,
            physical_padding,
            pbm.border.to_physical(writing_mode),
            pbm.margin.auto_is(Au::zero).to_physical(writing_mode),
            None,
        )
        .with_baselines(baselines);
        MathLayoutOutput {
            fragments: vec![Fragment::Box(box_fragment.into())],
            inline_size: total_inline,
            block_size: total_block,
            ascent: row_baseline,
            descent: row_max_descent + physical_padding.bottom,
        }
    }
}