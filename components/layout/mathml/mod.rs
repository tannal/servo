/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

mod layout;

pub(crate) use self::layout::{
    MathBoxMetrics, MathLayoutResult, MathOperatorProperties, MathStretchAxis, MathStretchTarget,
};

pub(crate) mod mfrac;
pub(crate) mod mpadded;
pub(crate) mod mrow;
pub(crate) mod mspace;
pub(crate) mod operator;
pub(crate) mod text;

use crate::flow::{BlockContainer, BlockContainerBuilder, BlockFormattingContext};
use crate::formatting_contexts::{Baselines, IndependentFormattingContextContents};
use crate::geom::PhysicalVec;
use crate::mathml::mspace::MathSpaceBlock;
use app_units::Au;
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::context::SharedStyleContext;
use style::properties::ComputedValues;
use web_atoms::local_name;

use crate::cell::ArcRefCell;
use crate::context::LayoutContext;
use crate::dom::{BoxSlot, LayoutBox, WeakLayoutBox};
use crate::dom_traversal::{
    BoxTreeString, Contents, NodeAndStyleInfo, NonReplacedContents, TraversalHandler,
};
use crate::formatting_contexts::IndependentFormattingContext;
use crate::fragment_tree::{CollapsedBlockMargins, Fragment};
use crate::layout_box_base::{IndependentFormattingContextLayoutResult, LayoutBoxBase};
use crate::positioned::PositioningContext;
use crate::sizing::InlineContentSizesResult;
use crate::style_ext::{DisplayGeneratingBox, LayoutStyle};
use crate::{ConstraintSpace, ContainingBlock, PropagatedBoxTreeData};
use layout_api::{LayoutElement, LayoutNode};

pub(crate) use self::mfrac::MathFractionBlock;
pub(crate) use self::mpadded::MathPaddedBlock;
pub(crate) use self::mrow::MathRowBlock;
pub(crate) use self::operator::MathOperatorBlock;
pub(crate) use self::text::MathTextBlock;

pub(crate) struct MathTraversalBuilder<'a, 'dom> {
    pub context: &'a LayoutContext<'a>,
    pub children: Vec<ArcRefCell<MathLevelBox>>,
    pub propagated_data: PropagatedBoxTreeData,
    pub _marker: std::marker::PhantomData<&'dom ()>,
}

impl<'a, 'dom> MathTraversalBuilder<'a, 'dom> {
    pub fn new(context: &'a LayoutContext<'a>, propagated_data: PropagatedBoxTreeData) -> Self {
        Self {
            context,
            children: Vec::new(),
            propagated_data,
            _marker: std::marker::PhantomData,
        }
    }

    pub fn into_children(self) -> Vec<ArcRefCell<MathLevelBox>> {
        self.children
    }
}

impl<'a, 'dom> TraversalHandler<'dom> for MathTraversalBuilder<'a, 'dom> {
    fn handle_element(
        &mut self,
        info: &NodeAndStyleInfo<'dom>,
        _display: DisplayGeneratingBox,
        contents: Contents,
        box_slot: BoxSlot<'dom>,
    ) {
        let element = info.node.as_element();
        let local_name = element.as_ref().map(|elem| elem.local_name());

        match local_name {
            Some(&local_name!("mo")) => {
                let mut block_builder =
                    BlockContainerBuilder::new(self.context, info, self.propagated_data);
                if let Contents::NonReplaced(non_replaced) = contents {
                    non_replaced.traverse(self.context, info, &mut block_builder);
                }
                let level_box = match block_builder.finish() {
                    BlockContainer::InlineFormattingContext(ifc) => {
                        let operator_block = MathOperatorBlock::construct(info, ifc);
                        ArcRefCell::new(MathLevelBox::Operator(ArcRefCell::new(operator_block)))
                    },
                    block_container => {
                        // Fallback when <mo> is empty or forced into a BFC (e.g. by overflow: hidden)
                        let bfc = BlockFormattingContext::from_block_container(block_container);
                        let contents = IndependentFormattingContextContents::Flow(bfc);
                        let independent_fc = IndependentFormattingContext::new(
                            LayoutBoxBase::new(info.into(), info.style.clone()),
                            contents,
                            self.propagated_data,
                        );
                        let text_block = MathTextBlock {
                            base: LayoutBoxBase::new(info.into(), info.style.clone()),
                            formatting_context: independent_fc,
                            is_space_like: false,
                        };
                        ArcRefCell::new(MathLevelBox::Text(ArcRefCell::new(text_block)))
                    },
                };
                box_slot.set(LayoutBox::MathLevel(level_box.clone()));
                self.children.push(level_box);
            },
            Some(&local_name!("mi"))
            | Some(&local_name!("mn"))
            | Some(&local_name!("ms"))
            | Some(&local_name!("mtext")) => {
                // Integration point: Delegate to BlockContainerBuilder to support HTML/SVG subtrees inside mtext.
                let mut block_builder =
                    BlockContainerBuilder::new(self.context, info, self.propagated_data);

                if let Contents::NonReplaced(non_replaced) = contents {
                    non_replaced.traverse(self.context, info, &mut block_builder);
                }

                let block_container = block_builder.finish();
                let bfc = BlockFormattingContext::from_block_container(block_container);
                let contents = IndependentFormattingContextContents::Flow(bfc);

                let independent_fc = IndependentFormattingContext::new(
                    LayoutBoxBase::new(info.into(), info.style.clone()),
                    contents,
                    self.propagated_data,
                );

                let text_block = MathTextBlock {
                    base: LayoutBoxBase::new(info.into(), info.style.clone()),
                    formatting_context: independent_fc,
                    is_space_like: local_name == Some(&local_name!("mtext")),
                };
                let level_box = ArcRefCell::new(MathLevelBox::Text(ArcRefCell::new(text_block)));
                box_slot.set(LayoutBox::MathLevel(level_box.clone()));
                self.children.push(level_box);
            },

            _ => {
                // Other MathML container elements (mrow, mfrac, msqrt, etc.)
                // Standard MathML container recursion (<mrow>, <mpadded>, <mfrac>, etc.)
                let mut child_builder =
                    MathTraversalBuilder::new(self.context, self.propagated_data);
                if let Contents::NonReplaced(non_replaced) = contents {
                    non_replaced.traverse(self.context, info, &mut child_builder);
                }

                let child_children = child_builder.into_children();
                let level_box = MathLevelBox::construct(self.context, info, child_children);
                let level_box_cell = ArcRefCell::new(level_box);

                box_slot.set(LayoutBox::MathLevel(level_box_cell.clone()));
                self.children.push(level_box_cell);
            },
        }
    }

    fn handle_text(&mut self, info: &NodeAndStyleInfo<'dom>, text: BoxTreeString<'dom>) {}

    fn enter_display_contents(&mut self, styles: crate::flow::inline::SharedInlineStyles) {}

    fn leave_display_contents(&mut self) {}
}

/// Helper to translate a slice of fragments by a 2D offset.
pub(crate) fn translate_fragments(fragments: &mut [Fragment], offset_x: Au, offset_y: Au) {
    let translation = PhysicalVec::new(offset_x, offset_y);
    for fragment in fragments {
        if let Some(base) = fragment.base() {
            base.translate_rect(translation.into());
        }
    }
}

#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathContainer {
    pub base: LayoutBoxBase,
    pub root_box: ArcRefCell<MathLevelBox>,
    style: ServoArc<ComputedValues>,
}
impl MathContainer {
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
        let root_box = MathLevelBox::construct_subtree(
            context,
            node_and_style_info,
            contents,
            propagated_data,
        );
        Self {
            base,
            root_box: ArcRefCell::new(root_box),
            style: node_and_style_info.style.clone(),
        }
    }
    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        self.root_box.borrow().attached_to_tree(layout_box);
    }
    #[inline]
    pub(crate) fn layout_style(&self) -> LayoutStyle<'_> {
        LayoutStyle::Default(&self.style)
    }
    pub(crate) fn compute_inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.root_box
            .borrow()
            .inline_content_sizes(layout_context, constraint_space)
    }
    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
        self.root_box
            .borrow_mut()
            .repair_style(context, node, new_style);
    }
    pub(crate) fn subtree_size(&self) -> usize {
        self.root_box.borrow().subtree_size() + 1
    }
    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
    ) -> IndependentFormattingContextLayoutResult {
        // Run single unified layout on root math box
        let output = self.root_box.borrow().layout(
            layout_context,
            positioning_context,
            containing_block,
            None,
        );
        let baselines = Baselines {
            first: Some(output.metrics.ascent()),
            last: Some(output.metrics.ascent()),
        };
        IndependentFormattingContextLayoutResult {
            fragments: output.fragments,
            content_block_size: output.metrics.size.block,
            content_inline_size_for_table: None,
            baselines,
            depends_on_block_constraints: true,
            specific_layout_info: None,
            collapsible_margins_in_children: CollapsedBlockMargins::zero(),
        }
    }
}

#[derive(Debug, MallocSizeOf)]
pub(crate) enum MathLevelBox {
    Row(ArcRefCell<MathRowBlock>),
    Space(ArcRefCell<MathSpaceBlock>),
    Padded(ArcRefCell<MathPaddedBlock>),
    Fraction(ArcRefCell<MathFractionBlock>),
    Text(ArcRefCell<MathTextBlock>),
    Operator(ArcRefCell<MathOperatorBlock>),
}
macro_rules! dispatch_math_box {
    ($self:expr, borrow =>$method:ident ($($arg:expr),*)) => {
        match $self {
            MathLevelBox::Row(b) => b.borrow().$method($($arg),*),
            MathLevelBox::Space(b) => b.borrow().$method($($arg),*),
            MathLevelBox::Padded(b) => b.borrow().$method($($arg),*),
            MathLevelBox::Fraction(b) => b.borrow().$method($($arg),*),
            MathLevelBox::Text(b) => b.borrow().$method($($arg),*),
            MathLevelBox::Operator(b) => b.borrow().$method($($arg),*),
        }
    };
    ($self:expr, borrow_mut =>$method:ident ($($arg:expr),*)) => {
        match $self {
            MathLevelBox::Row(b) => b.borrow_mut().$method($($arg),*),
            MathLevelBox::Space(b) => b.borrow_mut().$method($($arg),*),
            MathLevelBox::Padded(b) => b.borrow_mut().$method($($arg),*),
            MathLevelBox::Fraction(b) => b.borrow_mut().$method($($arg),*),
            MathLevelBox::Text(b) => b.borrow_mut().$method($($arg),*),
            MathLevelBox::Operator(b) => b.borrow_mut().$method($($arg),*),
        }
    };
    ($self:expr, base =>$callback:expr) => {
        match $self {
            MathLevelBox::Row(b) => $callback(&b.borrow().base),
            MathLevelBox::Space(b) => $callback(&b.borrow().base),
            MathLevelBox::Padded(b) => $callback(&b.borrow().base),
            MathLevelBox::Fraction(b) => $callback(&b.borrow().inner_row.base),
            MathLevelBox::Text(b) => $callback(&b.borrow().base),
            MathLevelBox::Operator(b) => $callback(&b.borrow().base),
        }
    };
    ($self:expr, base_mut =>$callback:expr) => {
        match $self {
            MathLevelBox::Row(b) => $callback(&mut b.borrow_mut().base),
            MathLevelBox::Space(b) => $callback(&mut b.borrow_mut().base),
            MathLevelBox::Padded(b) => $callback(&mut b.borrow_mut().base),
            MathLevelBox::Fraction(b) => $callback(&mut b.borrow_mut().inner_row.base),
            MathLevelBox::Text(b) => $callback(&mut b.borrow_mut().base),
            MathLevelBox::Operator(b) => $callback(&mut b.borrow_mut().base),
        }
    };
}
impl MathLevelBox {
    pub(crate) fn is_in_flow(&self) -> bool {
        self.with_base(|base| !base.style.get_box().position.is_absolutely_positioned())
    }

    pub(crate) fn is_space_like(&self) -> bool {
        match self {
            Self::Space(_) => true,
            Self::Text(text) => text.borrow().is_space_like,
            Self::Row(row) => row.borrow().is_space_like(),
            Self::Padded(padded) => padded.borrow().inner_row.is_space_like(),
            _ => false,
        }
    }

    /// Structural communication is a query, independent of the layout result.
    /// Recompute it from the current tree rather than caching ancestor pointers.
    pub(crate) fn operator_properties(&self) -> Option<MathOperatorProperties> {
        match self {
            Self::Operator(operator) => Some(operator.borrow().operator_properties()),
            Self::Row(row) => row.borrow().operator_properties(),
            Self::Padded(padded) => padded.borrow().inner_row.operator_properties(),
            Self::Fraction(fraction) => {
                let fraction = fraction.borrow();
                if let Some(numerator) = &fraction.numerator {
                    return numerator.borrow().operator_properties();
                }
                fraction
                    .inner_row
                    .children
                    .iter()
                    .find_map(|child| {
                        let child = child.borrow();
                        child.is_in_flow().then(|| child.operator_properties())
                    })
                    .flatten()
            },
            _ => None,
        }
    }

    /// A parent's measurement request uses the child's intrinsic contribution
    /// and the parent's style as the child's containing-block style. Shared font
    /// and positioning services remain explicit arguments to layout.
    pub(crate) fn layout_for_math_parent(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        parent_style: &ComputedValues,
        stretch: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
        let constraint_space =
            ConstraintSpace::new(crate::sizing::SizeConstraint::default(), parent_style, None);
        let sizes = self.inline_content_sizes(layout_context, &constraint_space);
        let child_containing_block = ContainingBlock {
            size: crate::ContainingBlockSize {
                inline: sizes.sizes.max_content,
                block: containing_block.size.block,
            },
            style: parent_style,
        };
        self.layout(
            layout_context,
            positioning_context,
            &child_containing_block,
            stretch,
        )
    }
    pub(crate) fn construct_subtree(
        context: &LayoutContext,
        node_and_style_info: &NodeAndStyleInfo,
        contents: NonReplacedContents,
        propagated_data: PropagatedBoxTreeData,
    ) -> Self {
        let mut builder = MathTraversalBuilder::new(context, propagated_data);
        contents.traverse(context, node_and_style_info, &mut builder);
        let children = builder.into_children();
        Self::construct(context, node_and_style_info, children)
    }
    pub(crate) fn construct(
        context: &LayoutContext,
        node_and_style_info: &NodeAndStyleInfo,
        children: Vec<ArcRefCell<MathLevelBox>>,
    ) -> Self {
        let element = node_and_style_info.node.as_element();
        let local_name = element.as_ref().map(|el| el.local_name());
        match local_name {
            Some(name) if name == &local_name!("mpadded") => MathLevelBox::Padded(ArcRefCell::new(
                MathPaddedBlock::construct(context, node_and_style_info, children),
            )),
            Some(name) if name == &local_name!("mfrac") => MathLevelBox::Fraction(ArcRefCell::new(
                MathFractionBlock::construct(context, node_and_style_info, children),
            )),
            Some(name) if name == &local_name!("mspace") => MathLevelBox::Space(ArcRefCell::new(
                MathSpaceBlock::construct(context, node_and_style_info),
            )),
            _ => MathLevelBox::Row(ArcRefCell::new(MathRowBlock::construct(
                node_and_style_info,
                children,
            ))),
        }
    }
    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        dispatch_math_box!(self, borrow_mut => repair_style(context, node, new_style));
    }
    pub(crate) fn with_base<T>(&self, callback: impl FnOnce(&LayoutBoxBase) -> T) -> T {
        dispatch_math_box!(self, base => callback)
    }
    pub(crate) fn with_base_mut<T>(&mut self, callback: impl FnOnce(&mut LayoutBoxBase) -> T) -> T {
        dispatch_math_box!(self, base_mut => callback)
    }
    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        dispatch_math_box!(self, borrow => attached_to_tree(layout_box));
    }
    pub(crate) fn subtree_size(&self) -> usize {
        self.with_base(|base| base.subtree_size())
    }
    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        dispatch_math_box!(self, borrow => inline_content_sizes(layout_context, constraint_space))
    }
    /// Single unified layout dispatch for all MathML elements.
    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
        let output = match self {
            MathLevelBox::Row(b) => b.borrow().layout(
                layout_context,
                positioning_context,
                containing_block,
                target_size,
            ),
            MathLevelBox::Space(b) => b.borrow().layout(
                layout_context,
                positioning_context,
                containing_block,
                target_size,
            ),
            MathLevelBox::Fraction(b) => b.borrow().layout(
                layout_context,
                positioning_context,
                containing_block,
                target_size,
            ),
            MathLevelBox::Text(b) => b.borrow().layout(
                layout_context,
                positioning_context,
                containing_block,
                target_size,
            ),
            MathLevelBox::Operator(b) => b.borrow().layout(
                layout_context,
                positioning_context,
                containing_block,
                target_size,
            ),
            MathLevelBox::Padded(b) => b.borrow().layout(
                layout_context,
                positioning_context,
                containing_block,
                target_size,
            ),
        };

        self.with_base(|base| {
            base.clear_fragments();
            for fragment in &output.fragments {
                base.add_fragment(fragment.clone());
            }
        });

        output
    }
}
