/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use app_units::{Au, MAX_AU, MIN_AU};
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use std::sync::Arc;
use style::Zero;
use style::context::SharedStyleContext;
use style::logical_geometry::WritingMode;
use style::properties::ComputedValues;
use web_atoms::local_name;

use crate::cell::ArcRefCell;
use crate::context::LayoutContext;
use crate::dom::LayoutBox;
use crate::dom::{BoxSlot, WeakLayoutBox};
use crate::dom_traversal::{
    BoxTreeString, Contents, NodeAndStyleInfo, NonReplacedContents, TraversalHandler,
};
use crate::flow::{BlockContainerBuilder, BlockFormattingContext};
use crate::formatting_contexts::{
    Baselines, IndependentFormattingContext, IndependentFormattingContextContents,
};
use crate::fragment_tree::{
    BaseFragmentInfo, BoxFragment, CollapsedBlockMargins, Fragment, MathFragment, MathFragmentKind,
};
use crate::geom::{LogicalRect, LogicalVec2, PhysicalPoint, PhysicalVec};
use crate::layout_box_base::{IndependentFormattingContextLayoutResult, LayoutBoxBase};
use crate::positioned::PositioningContext;
use crate::sizing::{
    ComputeInlineContentSizes, ContentSizes, InlineContentSizesResult, LazySize, SizeConstraint,
};
use crate::style_ext::LayoutStyle;
use crate::wasm_layout::{self, CustomBoxMetrics};
use crate::{ConstraintSpace, ContainingBlock, ContainingBlockSize, PropagatedBoxTreeData};

#[derive(Debug, MallocSizeOf)]
pub(crate) enum WasmChildBox {
    Container(ArcRefCell<WasmContainer>),
    Flow(ArcRefCell<IndependentFormattingContext>),
}

impl WasmChildBox {
    pub fn with_base<T>(&self, callback: impl FnOnce(&LayoutBoxBase) -> T) -> T {
        match self {
            WasmChildBox::Container(c) => callback(&c.borrow().base),
            WasmChildBox::Flow(f) => callback(&f.borrow().base),
        }
    }

    pub fn with_base_mut<T>(&mut self, callback: impl FnOnce(&mut LayoutBoxBase) -> T) -> T {
        match self {
            WasmChildBox::Container(c) => callback(&mut c.borrow_mut().base),
            WasmChildBox::Flow(f) => callback(&mut f.borrow_mut().base),
        }
    }

    pub fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        match self {
            WasmChildBox::Container(c) => c.borrow().attached_to_tree(layout_box),
            WasmChildBox::Flow(f) => f.borrow().attached_to_tree(layout_box),
        }
    }

    pub fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        match self {
            WasmChildBox::Container(c) => c.borrow_mut().repair_style(context, node, new_style),
            WasmChildBox::Flow(f) => f.borrow_mut().repair_style(context, node, new_style),
        }
    }

    pub fn subtree_size(&self) -> usize {
        match self {
            WasmChildBox::Container(c) => c.borrow().subtree_size(),
            WasmChildBox::Flow(f) => f.borrow().subtree_size(),
        }
    }

    pub fn set_selection(&self, selected: bool) -> bool {
        match self {
            WasmChildBox::Container(c) => c.borrow().set_selection(selected),
            WasmChildBox::Flow(f) => f.borrow().set_selection(selected),
        }
    }

    pub fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        match self {
            WasmChildBox::Container(c) => c
                .borrow()
                .inline_content_sizes(layout_context, constraint_space),
            WasmChildBox::Flow(f) => f
                .borrow()
                .inline_content_sizes(layout_context, constraint_space),
        }
    }

    pub fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
    ) -> IndependentFormattingContextLayoutResult {
        match self {
            WasmChildBox::Container(c) => {
                c.borrow()
                    .layout(layout_context, positioning_context, containing_block)
            },
            WasmChildBox::Flow(f) => f.borrow().layout(
                layout_context,
                positioning_context,
                containing_block,
                containing_block,
                None,
                &LazySize::intrinsic(),
            ),
        }
    }
}

struct WasmBoxTraversalBuilder<'a, 'dom> {
    context: &'a LayoutContext<'a>,
    children: Vec<ArcRefCell<WasmChildBox>>,
    propagated_data: PropagatedBoxTreeData,
    _marker: std::marker::PhantomData<&'dom ()>,
}

impl<'a, 'dom> TraversalHandler<'dom> for WasmBoxTraversalBuilder<'a, 'dom> {
    fn handle_element(
        &mut self,
        info: &NodeAndStyleInfo<'dom>,
        _display: crate::style_ext::DisplayGeneratingBox,
        contents: Contents,
        box_slot: BoxSlot<'dom>,
    ) {
        let element = info.node.as_element();
        let local_name = element.as_ref().map(|elem| elem.local_name());

        match local_name {
            Some(&local_name!("mo"))
            | Some(&local_name!("mi"))
            | Some(&local_name!("mn"))
            | Some(&local_name!("mtext")) => {
                let mut block_builder =
                    BlockContainerBuilder::new(self.context, info, self.propagated_data);
                if let Contents::NonReplaced(non_replaced) = contents {
                    non_replaced.traverse(self.context, info, &mut block_builder);
                }
                let block_container = block_builder.finish();
                let bfc = BlockFormattingContext::from_block_container(block_container);
                let flow = IndependentFormattingContext::new(
                    LayoutBoxBase::new(info.into(), info.style.clone()),
                    IndependentFormattingContextContents::Flow(bfc),
                    self.propagated_data,
                );
                let child = ArcRefCell::new(WasmChildBox::Flow(ArcRefCell::new(flow)));
                box_slot.set(LayoutBox::WasmLevel(child.clone()));
                self.children.push(child);
            },
            _ => {
                let non_replaced = match contents {
                    Contents::NonReplaced(c) => c,
                    _ => NonReplacedContents::OfElement,
                };
                let container = WasmContainer::construct(
                    self.context,
                    info,
                    non_replaced,
                    self.propagated_data,
                );
                let child = ArcRefCell::new(WasmChildBox::Container(ArcRefCell::new(container)));
                box_slot.set(LayoutBox::WasmLevel(child.clone()));
                self.children.push(child);
            },
        }
    }

    fn handle_text(&mut self, _info: &NodeAndStyleInfo<'dom>, _text: BoxTreeString<'dom>) {}
    fn enter_display_contents(&mut self, _styles: crate::flow::inline::SharedInlineStyles) {}
    fn leave_display_contents(&mut self) {}
}

#[derive(Debug, MallocSizeOf)]
pub(crate) struct WasmContainer {
    pub base: LayoutBoxBase,
    pub tag_name: String,
    pub children: Vec<ArcRefCell<WasmChildBox>>,
    style: ServoArc<ComputedValues>,
}

impl WasmContainer {
    pub(crate) fn construct(
        context: &LayoutContext,
        node_and_style_info: &NodeAndStyleInfo,
        contents: NonReplacedContents,
        propagated_data: PropagatedBoxTreeData,
    ) -> Self {
        let base = LayoutBoxBase::new(
            node_and_style_info.into(),
            node_and_style_info.style.clone(),
        );
        let tag_name = node_and_style_info
            .node
            .as_element()
            .map(|e| e.local_name().to_string())
            .unwrap_or_else(|| "div".to_string());

        let mut builder = WasmBoxTraversalBuilder {
            context,
            children: Vec::new(),
            propagated_data,
            _marker: std::marker::PhantomData,
        };
        contents.traverse(context, node_and_style_info, &mut builder);

        Self {
            base,
            tag_name,
            children: builder.children,
            style: node_and_style_info.style.clone(),
        }
    }

    #[inline]
    pub(crate) fn layout_style(&self) -> LayoutStyle<'_> {
        LayoutStyle::Default(&self.style)
    }

    pub(crate) fn subtree_size(&self) -> usize {
        self.children
            .iter()
            .map(|c| c.borrow().subtree_size())
            .sum::<usize>()
            + 1
    }

    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        for child in &self.children {
            child.borrow().attached_to_tree(layout_box.clone());
        }
    }

    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
        self.style = new_style.clone();
        for child in &self.children {
            child.borrow_mut().repair_style(context, node, new_style);
        }
    }

    pub fn set_selection(&self, selected: bool) -> bool {
        let mut changed = false;
        for child in &self.children {
            changed |= child.borrow().set_selection(selected);
        }
        changed
    }

    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        let is_vertical = matches!(
            self.tag_name.as_str(),
            "mfrac" | "munder" | "mover" | "munderover"
        );

        let mut min_content = Au::zero();
        let mut max_content = Au::zero();
        let mut depends_on_block_constraints = false;

        for child in &self.children {
            let child_sizes = child
                .borrow()
                .inline_content_sizes(layout_context, constraint_space);

            if is_vertical {
                min_content = min_content.max(child_sizes.sizes.min_content);
                max_content = max_content.max(child_sizes.sizes.max_content);
            } else {
                min_content += child_sizes.sizes.min_content;
                max_content += child_sizes.sizes.max_content;
            }
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

    fn resolve_font_metrics(layout_context: &LayoutContext, style: &ComputedValues) -> (Au, Au) {
        let font_size: Au = style.clone_font_size().computed_size().into();
        let font = layout_context.font_context.font_group(style.clone_font());
        let math_axis = font
            .first(&layout_context.font_context)
            .and_then(|f| f.metrics.get().map(|m| m.ascent / 2))
            .unwrap_or(font_size / 3);
        (font_size, math_axis)
    }

    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
    ) -> IndependentFormattingContextLayoutResult {
        let style = &self.style;
        let writing_mode = style.writing_mode;
        let pbm = LayoutStyle::Default(style)
            .content_box_sizes_and_padding_border_margin(&containing_block.into())
            .pbm;
        let physical_padding = pbm.padding.to_physical(writing_mode);

        let constraint_space = ConstraintSpace::new(SizeConstraint::default(), style, None);
        let mut child_outputs = Vec::with_capacity(self.children.len());
        let mut children_metrics = Vec::with_capacity(self.children.len());

        // 1. Layout children with intrinsic containing block constraints
        for child in &self.children {
            let content_sizes = child
                .borrow()
                .inline_content_sizes(layout_context, &constraint_space);

            let child_containing_block = ContainingBlock {
                size: ContainingBlockSize {
                    inline: content_sizes.sizes.max_content,
                    block: containing_block.size.block,
                },
                style: containing_block.style,
            };

            let child_layout =
                child
                    .borrow()
                    .layout(layout_context, positioning_context, &child_containing_block);

            let mut frag_inline_width = Au::zero();
            for frag in &child_layout.fragments {
                if let Some(base) = frag.base() {
                    let rect = base.rect();
                    frag_inline_width = frag_inline_width.max(rect.origin.x + rect.size.width);
                }
            }

            let child_width = if frag_inline_width > Au::zero() {
                frag_inline_width
            } else {
                content_sizes.sizes.max_content
            };

            let ascent = child_layout
                .baselines
                .first
                .unwrap_or(child_layout.content_block_size);
            let descent = (child_layout.content_block_size - ascent).max(Au::zero());

            children_metrics.push(CustomBoxMetrics {
                width: child_width,
                height: child_layout.content_block_size,
                ascent,
                descent,
            });

            child_outputs.push(child_layout.fragments);
        }

        // 2. Query Wasm Layout
        let (font_size, math_axis) = Self::resolve_font_metrics(layout_context, style);

        let wasm_res = wasm_layout::layout_custom_element(
            &self.tag_name,
            &children_metrics,
            font_size,
            math_axis,
            Au::from_px(1),
            None,
        );

        // 3. Assemble and translate fragments
        let mut child_fragments = Vec::new();
        let (total_inline, total_block, baseline, _descent, extra_rects) =
            if let Some(res) = wasm_res {
                for (i, mut frags) in child_outputs.into_iter().enumerate() {
                    let (ox, oy) = res
                        .children_offsets
                        .get(i)
                        .copied()
                        .unwrap_or((Au::zero(), Au::zero()));
                    let translation =
                        PhysicalVec::new(physical_padding.left + ox, physical_padding.top + oy);
                    for frag in &mut frags {
                        if let Some(base) = frag.base() {
                            base.translate_rect(translation.into());
                        }
                    }
                    child_fragments.extend(frags);
                }
                let total_inline = res.width + physical_padding.left + physical_padding.right;
                let total_block = res.height + physical_padding.top + physical_padding.bottom;
                let baseline = physical_padding.top + res.ascent;
                (
                    total_inline,
                    total_block,
                    baseline,
                    res.descent + physical_padding.bottom,
                    res.extra_rects,
                )
            } else {
                // Graceful fallback: Lay out children as a standard inline row along the baseline
                let mut current_x = physical_padding.left;
                let mut max_ascent = Au::zero();
                let mut max_descent = Au::zero();
                for child in &children_metrics {
                    max_ascent = max_ascent.max(child.ascent);
                    max_descent = max_descent.max(child.descent);
                }
                for (i, mut frags) in child_outputs.into_iter().enumerate() {
                    let child_metrics = &children_metrics[i];
                    let y_offset = max_ascent - child_metrics.ascent;
                    let translation = PhysicalVec::new(current_x, physical_padding.top + y_offset);
                    for frag in &mut frags {
                        if let Some(base) = frag.base() {
                            base.translate_rect(translation.into());
                        }
                    }
                    current_x += child_metrics.width;
                    child_fragments.extend(frags);
                }
                let total_inline = current_x + physical_padding.right;
                let total_block = max_ascent + max_descent + physical_padding.top + physical_padding.bottom;
                let baseline = physical_padding.top + max_ascent;
                (
                    total_inline,
                    total_block,
                    baseline,
                    max_descent + physical_padding.bottom,
                    Vec::new(),
                )
            };

        let content_rect = LogicalRect {
            start_corner: LogicalVec2::zero(),
            size: LogicalVec2 {
                inline: total_inline,
                block: total_block,
            },
        }
        .as_physical(Some(containing_block));

        let box_frag = BoxFragment::new(
            self.base.base_fragment_info.clone(),
            self.base.style.clone(),
            child_fragments,
            content_rect,
            physical_padding,
            pbm.border.to_physical(writing_mode),
            pbm.margin.auto_is(Au::zero).to_physical(writing_mode),
            None,
        )
        .with_baselines(Baselines {
            first: Some(baseline),
            last: Some(baseline),
        });

        let bar_rect = extra_rects.first().map(|r| {
            r.translate(PhysicalPoint::new(physical_padding.left, physical_padding.top).to_vector())
        });

        IndependentFormattingContextLayoutResult {
            fragments: vec![Fragment::Math(Arc::new(MathFragment {
                box_fragment: box_frag.into(),
                kind: MathFragmentKind::Fraction { bar_rect },
            }))],
            content_block_size: total_block,
            content_inline_size_for_table: None,
            baselines: Baselines {
                first: Some(baseline),
                last: Some(baseline),
            },
            depends_on_block_constraints: false,
            specific_layout_info: None,
            collapsible_margins_in_children: CollapsedBlockMargins::zero(),
        }
    }
}

impl ComputeInlineContentSizes for WasmContainer {
    fn compute_inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.inline_content_sizes(layout_context, constraint_space)
    }
}
