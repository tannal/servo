use app_units::Au;
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::context::SharedStyleContext;
use style::properties::ComputedValues;
use style::values::computed::LengthPercentage;
// use style::values::specified::length::NonNegativeLength;
use std::sync::Arc;
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
use crate::formatting_contexts::Baselines;
use crate::fragment_tree::{BaseFragmentInfo, BoxFragment, Fragment};
use crate::geom::{LogicalRect, LogicalVec2};
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::MathLayoutOutput;
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult};
use crate::style_ext::LayoutStyle;
use crate::{ConstraintSpace, ContainingBlock};

#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathSpaceBlock {
    pub base: LayoutBoxBase,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    pub width: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    pub height: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    pub depth: Option<LengthPercentage>,
}

impl MathSpaceBlock {
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

    pub(crate) fn base(&self) -> &LayoutBoxBase {
        &self.base
    }

    pub(crate) fn base_mut(&mut self) -> &mut LayoutBoxBase {
        &mut self.base
    }

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
    pub(crate) fn attached_to_tree(&self, _layout_box: WeakLayoutBox) {}

    pub(crate) fn inline_content_sizes(
        &self,
        _layout_context: &LayoutContext,
        _constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        let width = self
            .width
            .as_ref()
            .map(|lp| lp.to_used_value(Au::zero()))
            .unwrap_or(Au::zero())
            .max(Au::zero());

        InlineContentSizesResult {
            sizes: ContentSizes {
                min_content: width,
                max_content: width,
            },
            depends_on_block_constraints: false,
        }
    }

    pub(crate) fn layout(
        &self,
        _layout_context: &LayoutContext,
        _positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        _target_size: Option<(Au, Au)>,
    ) -> MathLayoutOutput {
        let style = &self.base.style;
        let writing_mode = style.writing_mode;
        let layout_style = LayoutStyle::Default(style);
        let pbm = layout_style
            .content_box_sizes_and_padding_border_margin(&containing_block.into())
            .pbm;
        let physical_padding = pbm.padding.to_physical(writing_mode);
        let physical_border = pbm.border.to_physical(writing_mode);
        // Resolve width against containing block inline size
        // For width: resolve percentage against containing block inline size
        let width = self
            .width
            .as_ref()
            .map(|lp| lp.to_used_value(containing_block.size.inline))
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
        let padding_border_inline = physical_padding.left
            + physical_padding.right
            + physical_border.left
            + physical_border.right;
        let padding_border_block = physical_padding.top
            + physical_padding.bottom
            + physical_border.top
            + physical_border.bottom;
        let total_inline = width + padding_border_inline;
        let total_block = height + depth + padding_border_block;
        // Content box: size is (width, height + depth), offset by padding-start + border-start
        let content_rect = LogicalRect {
            start_corner: LogicalVec2 {
                inline: pbm.padding.inline_start + pbm.border.inline_start,
                block: pbm.padding.block_start + pbm.border.block_start,
            },
            size: LogicalVec2 {
                inline: width,
                block: height + depth,
            },
        }
        .as_physical(Some(containing_block));
        // Baseline: distance from box top (including border and padding) to baseline
        let baseline = physical_padding.top + physical_border.top + height;
        let baselines = Baselines {
            first: Some(baseline),
            last: Some(baseline),
        };
        let box_fragment = BoxFragment::new(
            self.base.base_fragment_info.clone(),
            self.base.style.clone(),
            Vec::new(), // mspace has no child fragments
            content_rect,
            physical_padding,
            physical_border,
            pbm.margin.auto_is(Au::zero).to_physical(writing_mode),
            None,
        )
        .with_baselines(baselines);
        MathLayoutOutput {
            fragments: vec![Fragment::Box(Arc::new(box_fragment))],
            inline_size: total_inline,
            block_size: total_block,
            ascent: baseline,
            descent: depth + physical_padding.bottom + physical_border.bottom,
        }
    }
}
