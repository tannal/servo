/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! MathML Core box construction, recursive layout dispatch, and incremental caching.
//!
//! Communication has three lifetimes: queries describe the current box tree,
//! arguments carry a parent's constraints (`MathStretchTarget`), and
//! `MathLayoutResult` returns geometry (`MathBoxMetrics` + fragments).
//!
//! Target specification: [MathML Core](https://w3c.github.io/mathml-core/).

mod layout;

pub(crate) use self::layout::{
    MathBoxMetrics, MathLayoutCache, MathLayoutResult, MathOperatorProperties, MathStretchAxis,
    MathStretchTarget,
};

pub(crate) mod mfrac;
pub(crate) mod mpadded;
pub(crate) mod mroot;
pub(crate) mod mrow;
pub(crate) mod mscripts;
pub(crate) mod mspace;
pub(crate) mod munder_mover;
pub(crate) mod operator;
pub(crate) mod operator_dictionary;
pub(crate) mod text;

use app_units::Au;
use layout_api::{LayoutElement, LayoutNode};
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
use crate::flow::{BlockContainer, BlockContainerBuilder, BlockFormattingContext};
use crate::formatting_contexts::{
    Baselines, IndependentFormattingContext, IndependentFormattingContextContents,
};
use crate::fragment_tree::{CollapsedBlockMargins, Fragment};
use crate::geom::PhysicalVec;
use crate::layout_box_base::{IndependentFormattingContextLayoutResult, LayoutBoxBase};
use crate::positioned::PositioningContext;
use crate::sizing::InlineContentSizesResult;
use crate::style_ext::{DisplayGeneratingBox, DisplayInside, LayoutStyle};
use crate::{ConstraintSpace, ContainingBlock, PropagatedBoxTreeData};

pub(crate) use self::mfrac::MathFractionBlock;
pub(crate) use self::mpadded::MathPaddedBlock;
pub(crate) use self::mroot::MathRadicalBlock;
pub(crate) use self::mrow::MathRowBlock;
pub(crate) use self::mscripts::{MathScriptsBlock, MathScriptsKind};
pub(crate) use self::mspace::MathSpaceBlock;
pub(crate) use self::munder_mover::{MathUnderOverBlock, MathUnderOverKind};
pub(crate) use self::operator::MathOperatorBlock;
pub(crate) use self::text::MathTextBlock;

/// Construction-time adapter from DOM/style traversal into mathematical children.
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
        display: DisplayGeneratingBox,
        contents: Contents,
        box_slot: BoxSlot<'dom>,
    ) {
        let element = info.node.as_element();
        let local_name = element.as_ref().map(|elem| elem.local_name());

        // Non-math CSS formatting contexts (including `<mtable>` with `display: inline-table`,
        // HTML elements with `display: block/flex/grid/table`, and replaced elements) delegate
        // directly to Servo's `IndependentFormattingContext`.
        let non_math_display = match display {
            DisplayGeneratingBox::OutsideInside { inside, .. } if inside != DisplayInside::Math => {
                Some(inside)
            },
            DisplayGeneratingBox::LayoutInternal(_) => {
                Some(DisplayInside::Flow { is_list_item: false })
            },
            _ => None,
        };

        if let Some(display_inside) = non_math_display
            .or_else(|| matches!(contents, Contents::Replaced(_)).then_some(DisplayInside::Flow { is_list_item: false }))
        {
            let independent_fc = IndependentFormattingContext::construct(
                self.context,
                info,
                display_inside,
                contents,
                self.propagated_data,
            );
            let is_mtable = local_name == Some(&local_name!("mtable"));
            let is_space_like = local_name == Some(&local_name!("mtext"));
            let is_token = matches!(
                local_name,
                Some(&local_name!("mi"))
                    | Some(&local_name!("mn"))
                    | Some(&local_name!("ms"))
                    | Some(&local_name!("mtext"))
            );
            let text_block =
                MathTextBlock::construct(info, independent_fc, is_space_like, is_token, is_mtable);
            let level_box = ArcRefCell::new(MathLevelBox::Text(ArcRefCell::new(text_block)));
            box_slot.set(LayoutBox::MathLevel(level_box.clone()));
            self.children.push(level_box);
            return;
        }

        match local_name {
            Some(&local_name!("mo")) => {
                let mut block_builder =
                    BlockContainerBuilder::new(self.context, info, self.propagated_data);
                if let Contents::NonReplaced(non_replaced) = contents {
                    non_replaced.traverse(self.context, info, &mut block_builder);
                }
                let level_box = match block_builder.finish() {
                    BlockContainer::InlineFormattingContext(ifc) => {
                        let operator_block = MathOperatorBlock::construct(self.context, info, ifc);
                        ArcRefCell::new(MathLevelBox::Operator(ArcRefCell::new(operator_block)))
                    },
                    block_container => {
                        let bfc = BlockFormattingContext::from_block_container(block_container);
                        let contents = IndependentFormattingContextContents::Flow(bfc);
                        let independent_fc = IndependentFormattingContext::new(
                            LayoutBoxBase::new(info.into(), info.style.clone()),
                            contents,
                            self.propagated_data,
                        );
                        let text_block =
                            MathTextBlock::construct(info, independent_fc, false, false, false);
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

                let text_block = MathTextBlock::construct(
                    info,
                    independent_fc,
                    local_name == Some(&local_name!("mtext")),
                    true,
                    false,
                );
                let level_box = ArcRefCell::new(MathLevelBox::Text(ArcRefCell::new(text_block)));
                box_slot.set(LayoutBox::MathLevel(level_box.clone()));
                self.children.push(level_box);
            },

            _ => {
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

    fn handle_text(&mut self, _info: &NodeAndStyleInfo<'dom>, _text: BoxTreeString<'dom>) {}
    fn enter_display_contents(&mut self, _styles: crate::flow::inline::SharedInlineStyles) {}
    fn leave_display_contents(&mut self) {}
}

/// Translate top-level fragment origins in physical coordinates.
pub(crate) fn translate_fragments(fragments: &mut [Fragment], offset_x: Au, offset_y: Au) {
    let translation = PhysicalVec::new(offset_x, offset_y);
    for fragment in fragments {
        if let Some(base) = fragment.base() {
            base.translate_rect(translation.into());
        }
    }
}

/// Boundary between Servo's ordinary independent formatting context and the
/// recursive mathematical algorithms.
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
        self.style = new_style.clone();
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

/// Supported MathML Core box-tree algorithm variants.
#[derive(Debug, MallocSizeOf)]
pub(crate) enum MathLevelBox {
    /// Explicit/inferred row (`mrow`, `math`, `mstyle`, `mphantom`, `merror`, `maction`, `semantics`).
    Row(ArcRefCell<MathRowBlock>),
    /// Explicit dimensions without glyph content (`mspace`).
    Space(ArcRefCell<MathSpaceBlock>),
    /// Declared dimensions and displacement around an inferred row (`mpadded`).
    Padded(ArcRefCell<MathPaddedBlock>),
    /// Two-child fraction or invalid-arity row fallback (`mfrac`).
    Fraction(ArcRefCell<MathFractionBlock>),
    /// Square root or indexed radical (`msqrt`, `mroot`).
    Radical(ArcRefCell<MathRadicalBlock>),
    /// Subscript, superscript, sub-superscript, or multiscripts (`msub`, `msup`, `msubsup`, `mmultiscripts`).
    Scripts(ArcRefCell<MathScriptsBlock>),
    /// Underscript, overscript, or under-overscript (`munder`, `mover`, `munderover`).
    UnderOver(ArcRefCell<MathUnderOverBlock>),
    /// Token content (`mi`, `mn`, `ms`, `mtext`), `<mtable>`, or embedded CSS/replaced content.
    Text(ArcRefCell<MathTextBlock>),
    /// Operator-specific capability and OpenType MATH glyph stretching (`mo`).
    Operator(ArcRefCell<MathOperatorBlock>),
}

macro_rules! dispatch_math_box {
    ($self:expr, borrow => $method:ident ($($arg:expr),*)) => {
        match $self {
            MathLevelBox::Row(b) => b.borrow().$method($($arg),*),
            MathLevelBox::Space(b) => b.borrow().$method($($arg),*),
            MathLevelBox::Padded(b) => b.borrow().$method($($arg),*),
            MathLevelBox::Fraction(b) => b.borrow().$method($($arg),*),
            MathLevelBox::Radical(b) => b.borrow().$method($($arg),*),
            MathLevelBox::Scripts(b) => b.borrow().$method($($arg),*),
            MathLevelBox::UnderOver(b) => b.borrow().$method($($arg),*),
            MathLevelBox::Text(b) => b.borrow().$method($($arg),*),
            MathLevelBox::Operator(b) => b.borrow().$method($($arg),*),
        }
    };
    ($self:expr, borrow_mut => $method:ident ($($arg:expr),*)) => {
        match $self {
            MathLevelBox::Row(b) => b.borrow_mut().$method($($arg),*),
            MathLevelBox::Space(b) => b.borrow_mut().$method($($arg),*),
            MathLevelBox::Padded(b) => b.borrow_mut().$method($($arg),*),
            MathLevelBox::Fraction(b) => b.borrow_mut().$method($($arg),*),
            MathLevelBox::Radical(b) => b.borrow_mut().$method($($arg),*),
            MathLevelBox::Scripts(b) => b.borrow_mut().$method($($arg),*),
            MathLevelBox::UnderOver(b) => b.borrow_mut().$method($($arg),*),
            MathLevelBox::Text(b) => b.borrow_mut().$method($($arg),*),
            MathLevelBox::Operator(b) => b.borrow_mut().$method($($arg),*),
        }
    };
    ($self:expr, base => $callback:expr) => {
        match $self {
            MathLevelBox::Row(b) => $callback(&b.borrow().base),
            MathLevelBox::Space(b) => $callback(&b.borrow().base),
            MathLevelBox::Padded(b) => $callback(&b.borrow().base),
            MathLevelBox::Fraction(b) => $callback(&b.borrow().inner_row.base),
            MathLevelBox::Radical(b) => $callback(&b.borrow().inner_row.base),
            MathLevelBox::Scripts(b) => $callback(&b.borrow().inner_row.base),
            MathLevelBox::UnderOver(b) => $callback(&b.borrow().inner_row.base),
            MathLevelBox::Text(b) => $callback(&b.borrow().base),
            MathLevelBox::Operator(b) => $callback(&b.borrow().base),
        }
    };
    ($self:expr, base_mut => $callback:expr) => {
        match $self {
            MathLevelBox::Row(b) => $callback(&mut b.borrow_mut().base),
            MathLevelBox::Space(b) => $callback(&mut b.borrow_mut().base),
            MathLevelBox::Padded(b) => $callback(&mut b.borrow_mut().base),
            MathLevelBox::Fraction(b) => $callback(&mut b.borrow_mut().inner_row.base),
            MathLevelBox::Radical(b) => $callback(&mut b.borrow_mut().inner_row.base),
            MathLevelBox::Scripts(b) => $callback(&mut b.borrow_mut().inner_row.base),
            MathLevelBox::UnderOver(b) => $callback(&mut b.borrow_mut().inner_row.base),
            MathLevelBox::Text(b) => $callback(&mut b.borrow_mut().base),
            MathLevelBox::Operator(b) => $callback(&mut b.borrow_mut().base),
        }
    };
}

impl MathLevelBox {
    pub(crate) fn is_in_flow(&self) -> bool {
        self.with_base(|base| !base.style.get_box().position.is_absolutely_positioned())
    }

    pub(crate) fn is_mprescripts(&self) -> bool {
        match self {
            Self::Row(row) => row.borrow().is_mprescripts,
            _ => false,
        }
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
            Self::Scripts(scripts) => scripts.borrow().operator_properties(),
            Self::UnderOver(under_over) => under_over.borrow().operator_properties(),
            _ => None,
        }
    }

    /// Propagate an inferred `OperatorForm` (`prefix`/`infix`/`postfix`) down the
    /// MathML Core §3.2.4.1 embellished-operator chain to its core `<mo>`.
    pub(crate) fn set_inferred_operator_form(&self, form: operator_dictionary::OperatorForm) {
        match self {
            Self::Operator(operator) => operator.borrow().set_inferred_form(form),
            Self::Row(row) => {
                let row = row.borrow();
                if let Some(index) = row.embellished_operator_index() {
                    row.children[index].borrow().set_inferred_operator_form(form);
                }
            },
            Self::Padded(padded) => {
                let padded = padded.borrow();
                if let Some(index) = padded.inner_row.embellished_operator_index() {
                    padded.inner_row.children[index]
                        .borrow()
                        .set_inferred_operator_form(form);
                }
            },
            Self::Fraction(fraction) => {
                let fraction = fraction.borrow();
                if let Some(numerator) = &fraction.numerator {
                    numerator.borrow().set_inferred_operator_form(form);
                } else if let Some(index) = fraction.inner_row.embellished_operator_index() {
                    fraction.inner_row.children[index]
                        .borrow()
                        .set_inferred_operator_form(form);
                }
            },
            Self::Scripts(scripts) => {
                let scripts = scripts.borrow();
                if let Some(base) = &scripts.base_child {
                    base.borrow().set_inferred_operator_form(form);
                } else if let Some(index) = scripts.inner_row.embellished_operator_index() {
                    scripts.inner_row.children[index]
                        .borrow()
                        .set_inferred_operator_form(form);
                }
            },
            Self::UnderOver(under_over) => {
                let under_over = under_over.borrow();
                if let Some(base) = &under_over.base_child {
                    base.borrow().set_inferred_operator_form(form);
                } else if let Some(index) = under_over.inner_row.embellished_operator_index() {
                    under_over.inner_row.children[index]
                        .borrow()
                        .set_inferred_operator_form(form);
                }
            },
            _ => {},
        }
    }

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
            Some(name) if name == &local_name!("msqrt") => MathLevelBox::Radical(ArcRefCell::new(
                MathRadicalBlock::construct(node_and_style_info, children, false),
            )),
            Some(name) if name == &local_name!("mroot") => MathLevelBox::Radical(ArcRefCell::new(
                MathRadicalBlock::construct(node_and_style_info, children, true),
            )),
            Some(name) if name == &local_name!("msub") => MathLevelBox::Scripts(ArcRefCell::new(
                MathScriptsBlock::construct(node_and_style_info, children, MathScriptsKind::Sub),
            )),
            Some(name) if name == &local_name!("msup") => MathLevelBox::Scripts(ArcRefCell::new(
                MathScriptsBlock::construct(node_and_style_info, children, MathScriptsKind::Sup),
            )),
            Some(name) if name == &local_name!("msubsup") => {
                MathLevelBox::Scripts(ArcRefCell::new(MathScriptsBlock::construct(
                    node_and_style_info,
                    children,
                    MathScriptsKind::SubSup,
                )))
            },
            Some(name) if name == &local_name!("mmultiscripts") => {
                MathLevelBox::Scripts(ArcRefCell::new(MathScriptsBlock::construct(
                    node_and_style_info,
                    children,
                    MathScriptsKind::MultiScripts,
                )))
            },
            Some(name) if name == &local_name!("munder") => {
                MathLevelBox::UnderOver(ArcRefCell::new(MathUnderOverBlock::construct(
                    node_and_style_info,
                    children,
                    MathUnderOverKind::Under,
                )))
            },
            Some(name) if name == &local_name!("mover") => {
                MathLevelBox::UnderOver(ArcRefCell::new(MathUnderOverBlock::construct(
                    node_and_style_info,
                    children,
                    MathUnderOverKind::Over,
                )))
            },
            Some(name) if name == &local_name!("munderover") => {
                MathLevelBox::UnderOver(ArcRefCell::new(MathUnderOverBlock::construct(
                    node_and_style_info,
                    children,
                    MathUnderOverKind::UnderOver,
                )))
            },
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
            MathLevelBox::Radical(b) => b.borrow().layout(
                layout_context,
                positioning_context,
                containing_block,
                target_size,
            ),
            MathLevelBox::Scripts(b) => b.borrow().layout(
                layout_context,
                positioning_context,
                containing_block,
                target_size,
            ),
            MathLevelBox::UnderOver(b) => b.borrow().layout(
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
