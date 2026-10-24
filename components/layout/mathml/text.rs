/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::Arc;

use app_units::Au;
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::Zero;
use style::context::SharedStyleContext;
use style::properties::ComputedValues;

use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::flow::IndependentFloatOrAtomicLayoutResult;
use crate::formatting_contexts::IndependentFormattingContext;
use crate::fragment_tree::Fragment;
use crate::geom::{LogicalVec2, ToLogical};
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::MathLayoutOutput;
use crate::positioned::{PositioningContext, PositioningContextLength};
use crate::sizing::InlineContentSizesResult;
use crate::style_ext::ComputedValuesExt;
use crate::{ConstraintSpace, ContainingBlock};

#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathTextBlock {
    pub base: LayoutBoxBase,
    pub formatting_context: IndependentFormattingContext,
}

impl MathTextBlock {
    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        // Math rows consume their children's margin-box sizes. Include CSS sizing
        // properties and decorations here, as well as during layout.
        self.formatting_context.outer_inline_content_sizes(
            layout_context,
            &constraint_space.into(),
            &LogicalVec2::zero(),
            false,
            false,
        )
    }

    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        _target_size: Option<(Au, Au)>,
    ) -> MathLayoutOutput {
        // TODO: Text-only tokens without break opportunities need the ink-metric
        // path specified by MathML Core, rather than CSS line-box metrics.
        // IndependentFormattingContext::layout lays out content only. Use the
        // atomic-box wrapper to resolve CSS sizes and create the styled box,
        // keeping child fragments relative to its content area.
        let mut child_positioning_context = PositioningContext::default();
        let IndependentFloatOrAtomicLayoutResult {
            mut fragment,
            baselines,
            pbm_sums,
        } = self.formatting_context.layout_float_or_atomic_inline(
            layout_context,
            &mut child_positioning_context,
            containing_block,
        );
        let writing_mode = containing_block.style.writing_mode;
        let content_size = fragment.content_rect().size.to_logical(writing_mode);
        let size = content_size + pbm_sums.sum();
        let ascent = baselines
            .first
            .map_or(size.block, |baseline| pbm_sums.block_start + baseline);

        // The math parent places the margin box. BoxFragment stores the content
        // rectangle, and its baselines are relative to that content rectangle.
        fragment
            .base
            .translate_rect(pbm_sums.start_offset().to_physical_size(writing_mode));
        fragment = fragment.with_baselines(baselines);
        if fragment
            .style()
            .establishes_containing_block_for_absolute_descendants(fragment.base.flags)
        {
            child_positioning_context.layout_collected_children(layout_context, &mut fragment);
        }
        let fragment = Fragment::Box(Arc::new(fragment));
        child_positioning_context.adjust_static_position_of_hoisted_fragments(
            &fragment,
            PositioningContextLength::zero(),
        );
        positioning_context.append(child_positioning_context);

        MathLayoutOutput {
            fragments: vec![fragment],
            inline_size: size.inline,
            block_size: size.block,
            ascent,
            descent: size.block - ascent,
        }
    }

    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        self.formatting_context.attached_to_tree(layout_box);
    }

    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
        self.formatting_context
            .repair_style(context, node, new_style);
    }
}
