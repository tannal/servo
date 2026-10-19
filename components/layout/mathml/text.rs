/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::Arc;

use app_units::Au;
use fonts::{FontRef, ShapedText, ShapedTextSlice, ShapingOptions};
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use servo_base::text::Utf32CodeUnits;
use style::Zero;
use style::context::SharedStyleContext;
use style::dom::NodeInfo;
use style::logical_geometry::{Direction, LogicalAxis};
use style::properties::ComputedValues;
use web_atoms::{local_name, ns};

use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::dom_traversal::NodeAndStyleInfo;
use crate::flow::inline::InlineFormattingContext;
use crate::flow::inline::text_run::{FontAndScriptInfo, SharedTextRunData};
use crate::formatting_contexts::{Baselines, IndependentFormattingContext};
use crate::fragment_tree::{BaseFragment, BoxFragment, Fragment, TextFragment};
use crate::geom::{LogicalVec2, PhysicalPoint, PhysicalRect};
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::MathLayoutOutput;
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult, LazySize, Size};
use crate::style_ext::LayoutStyle;
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
        self.formatting_context
            .inline_content_sizes(layout_context, constraint_space)
    }

    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        _target_size: Option<(Au, Au)>,
    ) -> MathLayoutOutput {
        let sizes = Default::default();

        let lazy_block_size = LazySize::new(
            &sizes,
            LogicalAxis::Block,
            Size::FitContent,
            Au::zero,
            None,
            false,
        );

        let result = self.formatting_context.layout(
            layout_context,
            positioning_context,
            containing_block,
            containing_block,
            None,
            &lazy_block_size,
            
        );
        let ascent = result.baselines.first.unwrap_or(result.content_block_size);
        let descent = result.content_block_size - ascent;
        let mut inline_size = Au::zero();
        for frag in &result.fragments {
            if let Some(base) = frag.base() {
                let rect = base.rect();
                inline_size = inline_size.max(rect.origin.x + rect.size.width);
            }
        }
        if inline_size.is_zero() {
            inline_size = containing_block.size.inline;
        }
        MathLayoutOutput {
            fragments: result.fragments,
            inline_size,
            block_size: result.content_block_size,
            ascent,
            descent,
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
