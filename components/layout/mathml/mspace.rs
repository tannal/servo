//! An explicit mathematical space has dimensions even though it has no glyph.
//!
//! See [Core mspace](https://w3c.github.io/mathml-core/#space-mspace).
//! The TeX names height/depth describe distances on opposite sides of a baseline;
//! CSS padding, borders, and margins are additional box geometry, not glyph metrics.
//! The current attribute parsing and percentage bases below are implementation
//! choices to audit against that section, not a full implementation of CSS sizing.

use app_units::Au;
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::context::SharedStyleContext;
use style::properties::ComputedValues;
use style::values::computed::LengthPercentage;
// use style::values::specified::length::NonNegativeLength;
use style::Zero;
use style::attr::AttrValue;
use style::context::TreeCountingCaches;
use style::dom::DummyElementContext;
use style::properties::StyleBuilder;
use style::rule_cache::RuleCacheConditions;
use style::rule_tree::RuleCascadeFlags;
use style::stylesheets::container_rule::ContainerSizeQuery;
use style::values::computed::{Context, ToComputedValue};
use web_atoms::{local_name, ns};

use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::dom_traversal::NodeAndStyleInfo;
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{MathBoxMetrics, MathLayoutResult, MathStretchTarget};
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult};
use crate::style_ext::LayoutStyle;
use crate::containing_block::{ConstraintSpace, ContainingBlock};

/// Explicit empty notation with declared dimensions and an alphabetic baseline.
/// It has no glyph or operands, but its ordinary CSS box can still paint.
#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathSpaceBlock {
    /// Styled empty box, which can still paint a background or border.
    pub base: LayoutBoxBase,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    /// Computed inline extent attribute; unresolved percentage kept until layout.
    pub width: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    /// Requested extent above the alphabetic baseline.
    pub height: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    /// Requested extent below the alphabetic baseline.
    pub depth: Option<LengthPercentage>,
}

impl MathSpaceBlock {
    /// Capture parsed dimensions under the current style. Even an empty element
    /// produces a styled box; there are no mathematical operands to measure.
    pub(crate) fn construct(
        context: &LayoutContext,
        node_and_style_info: &NodeAndStyleInfo,
    ) -> Self {
        let base = LayoutBoxBase::new(
            node_and_style_info.into(),
            node_and_style_info.style.clone(),
        );
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
        let parse_attr = |name| {
            element
                .as_ref()
                .and_then(|el| el.attribute(&ns!(), &name))
                // Guard: only call Stylo's accessor if it's actually a LengthPercentage
                .filter(|attr| matches!(attr, AttrValue::LengthPercentage(..)))
                .and_then(AttrValue::as_length_percentage)
                .map(|lp| lp.to_computed_value(&to_computed_context))
        };
        let width = parse_attr(local_name!("width"));
        let height = parse_attr(local_name!("height"));
        let depth = parse_attr(local_name!("depth"));

        MathSpaceBlock {
            base,
            width,
            height,
            depth,
        }
    }

    /// Borrow the styled identity shared with normal fragment construction.
    pub(crate) fn base(&self) -> &LayoutBoxBase {
        &self.base
    }

    /// Mutable styled-base access for existing tree lifecycle operations.
    pub(crate) fn base_mut(&mut self) -> &mut LayoutBoxBase {
        &mut self.base
    }

    /// Recompute font-relative dimension attributes after a style repair.
    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
        let rule_cache_conditions = &mut RuleCacheConditions::default();
        let mut tree_counting_caches = TreeCountingCaches::default();
        let style_builder = StyleBuilder::new(
            context.stylist.device(),
            Some(context.stylist),
            Some(new_style),
            None,
            None,
            false,
        );
        let to_computed_context = Context::new(
            style_builder,
            context.quirks_mode(),
            rule_cache_conditions,
            ContainerSizeQuery::none(),
            RuleCascadeFlags::empty(),
            &DummyElementContext,
            &mut tree_counting_caches,
        );
        let element = node.as_element();
        let parse_attr = |name| {
            element
                .as_ref()
                .and_then(|el| el.attribute(&ns!(), &name))
                .and_then(|attr| match attr {
                    AttrValue::LengthPercentage(_, lp) => lp.as_ref(),
                    _ => None,
                })
                .map(|lp| lp.to_computed_value(&to_computed_context))
        };
        self.width = parse_attr(local_name!("width"));
        self.height = parse_attr(local_name!("height"));
        self.depth = parse_attr(local_name!("depth"));
    }
    /// No descendant ownership propagation is needed for this empty math box.
    pub(crate) fn attached_to_tree(&self, _layout_box: WeakLayoutBox) {}

    /// Compute the current outer fixed-space contribution. Attribute percentage
    /// resolution is deferred with a zero intrinsic basis; decorations are additional.
    pub(crate) fn inline_content_sizes(
        &self,
        _layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.base.cached_inline_content_sizes_with(constraint_space, || {
            let width = self
                .width
                .as_ref()
                .map(|lp| lp.to_used_value(Au::zero()))
                .unwrap_or(Au::zero())
                .max(Au::zero());

            let style = &self.base.style;
            let writing_mode = style.writing_mode;
            let layout_style = LayoutStyle::Default(style);
            let pbm = layout_style
                .content_box_sizes_and_padding_border_margin(&constraint_space.into())
                .pbm;
            let physical_padding = pbm.padding.to_physical(writing_mode);
            let physical_border = pbm.border.to_physical(writing_mode);
            let physical_margin = pbm.margin.auto_is(Au::zero).to_physical(writing_mode);
            let extra_inline = physical_padding.left
                + physical_padding.right
                + physical_border.left
                + physical_border.right
                + physical_margin.left
                + physical_margin.right;
            let total = width + extra_inline;

            InlineContentSizesResult {
                sizes: ContentSizes {
                    min_content: total,
                    max_content: total,
                },
                depends_on_block_constraints: false,
            }
        })
    }

    /// Resolve the current width/height/depth representation, place the baseline
    /// at height within the content box, and let the shared wrapper add CSS geometry.
    /// The stretch argument is intentionally ignored: an explicit space is not a glyph.
    pub(crate) fn layout(
        &self,
        _layout_context: &LayoutContext,
        _positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        _target_size: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
        let style = &self.base.style;
        let layout_style = LayoutStyle::Default(style);
        let pbm = layout_style
            .content_box_sizes_and_padding_border_margin(&containing_block.into())
            .pbm;
        // Resolve width against containing block inline size
        // For width: resolve percentage against containing block inline size
        let width = self
            .width
            .as_ref()
            .map(|lp| lp.to_used_value(containing_block.inline_size()))
            .unwrap_or(Au::zero())
            .max(Au::zero());
        // For height and depth: percentage basis is 0 per MathML Core §3.2.3
        let height = self
            .height
            .as_ref()
            .map(|lp| lp.to_used_value(Au::zero()))
            .unwrap_or(Au::zero())
            .max(Au::zero());
        let depth = self
            .depth
            .as_ref()
            .map(|lp| lp.to_used_value(Au::zero()))
            .unwrap_or(Au::zero())
            .max(Au::zero());

        MathLayoutResult::from_content(
            &self.base,
            containing_block,
            &pbm,
            Vec::new(),
            MathBoxMetrics::new(width, height + depth, height),
            None,
        )
    }
}
