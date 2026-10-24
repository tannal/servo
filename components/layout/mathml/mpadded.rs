use app_units::Au;
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::attr::AttrValue;
use style::context::{SharedStyleContext, TreeCountingCaches};
use style::dom::DummyElementContext;
use style::logical_geometry::LogicalSize;
use style::parser::{Parse, ParserContext};
use style::properties::{ComputedValues, StyleBuilder};
use style::rule_cache::RuleCacheConditions;
use style::rule_tree::RuleCascadeFlags;
use style::stylesheets::container_rule::ContainerSizeQuery;
// use style::values::specified::LengthPercentage;
// use style::values::computed::LengthPercentage;
use style::values::computed::{Context, LengthPercentage, ToComputedValue};
use web_atoms::local_name;
use web_atoms::ns;

use super::{MathLevelBox, MathTraversalBuilder};
use crate::cell::ArcRefCell;
use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::dom_traversal::{Contents, NodeAndStyleInfo};
use crate::fragment_tree::{BaseFragmentInfo, BoxFragment, Fragment};
use crate::geom::{LogicalRect, LogicalVec2, PhysicalSides, PhysicalVec};
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{MathRowBlock, translate_fragments};
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult};
use crate::style_ext::LayoutStyle;
use crate::{ConstraintSpace, ContainingBlock, PropagatedBoxTreeData};
use style::Zero;

#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathPaddedBlock {
    pub base: LayoutBoxBase,
    /// Inner mrow container used for initial layout & metric resolution
    pub inner_row: MathRowBlock,

    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    pub lspace: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    pub voffset: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    pub width: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    pub height: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    pub depth: Option<LengthPercentage>,
}

impl MathPaddedBlock {
    pub(crate) fn construct(
        context: &LayoutContext,
        node_and_style_info: &NodeAndStyleInfo,
        children: Vec<ArcRefCell<MathLevelBox>>,
    ) -> MathPaddedBlock {
        let base_fragment_info: BaseFragmentInfo = node_and_style_info.into();
        let base = LayoutBoxBase::new(base_fragment_info, node_and_style_info.style.clone());

        let element = node_and_style_info.node.as_element();

        // 1. 构建 Stylo ToComputedContext 解析 mpadded 属性
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
                .and_then(|attr| match attr {
                    AttrValue::LengthPercentage(_, lp) => lp.as_ref(),
                    _ => None,
                })
                .map(|lp| lp.to_computed_value(&to_computed_context))
        };

        let lspace = parse_attr(local_name!("lspace"));
        let voffset = parse_attr(local_name!("voffset"));
        let width = parse_attr(local_name!("width"));
        let height = parse_attr(local_name!("height"));
        let depth = parse_attr(local_name!("depth"));

        // 2. 根据传入的 children 计算 Layout Subtree Size
        let subtree_size = children
            .iter()
            .map(|c| c.borrow().with_base(|base| base.subtree_size()))
            .sum::<usize>()
            + 1;
        base.set_subtree_size(subtree_size);

        let inner_row = MathRowBlock::construct(node_and_style_info, children);

        MathPaddedBlock {
            base,
            inner_row,
            lspace,
            voffset,
            width,
            height,
            depth,
        }
    }

    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
        self.inner_row.repair_style(context, node, new_style);
    }

    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        self.inner_row.attached_to_tree(layout_box);
    }

    #[inline]
    fn resolve_length(&self, length: &Option<LengthPercentage>, base_val: Au) -> Option<Au> {
        length.as_ref().map(|lp| lp.to_used_value(base_val))
    }

    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        let inner_sizes = self
            .inner_row
            .inline_content_sizes(layout_context, constraint_space);

        let mut min_content = inner_sizes.sizes.min_content;
        let mut max_content = inner_sizes.sizes.max_content;

        // Apply requested width overrides if present
        if let Some(w) = self.resolve_length(&self.width, max_content) {
            min_content = w.max(Au::zero());
            max_content = w.max(Au::zero());
        }

        InlineContentSizesResult {
            sizes: ContentSizes {
                min_content,
                max_content,
            },
            depends_on_block_constraints: inner_sizes.depends_on_block_constraints,
        }
    }

    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
    ) -> Vec<Fragment> {
        let style = &self.base.style;
        let writing_mode = style.writing_mode;
        let is_rtl = !writing_mode.is_bidi_ltr();

        let layout_style = LayoutStyle::Default(style);
        let pbm = layout_style
            .content_box_sizes_and_padding_border_margin(&containing_block.into())
            .pbm;
        let physical_padding = pbm.padding.to_physical(writing_mode);

        // 1. Layout inner anonymous mrow
        let mut output = self
            .inner_row
            .layout(layout_context, positioning_context, containing_block, None);

        // 2. Measure inner content dimensions
        let mut inner_width = Au::zero();
        let mut inner_ascent = Au::zero();
        let mut inner_descent = Au::zero();

        let mut child_fragments = output.fragments;

        for fragment in &child_fragments {
            if let Some(base) = fragment.base() {
                let rect = base.rect();
                inner_width = inner_width.max(rect.origin.x + rect.size.width);
                inner_ascent = inner_ascent.max(rect.origin.y);
                inner_descent =
                    inner_descent.max((rect.origin.y + rect.size.height) - inner_ascent);
            }
        }

        // 3. Resolve attribute metric overrides
        let resolved_width = self
            .resolve_length(&self.width, inner_width)
            .unwrap_or(inner_width)
            .max(Au::zero());

        let resolved_ascent = self
            .resolve_length(&self.height, inner_ascent)
            .unwrap_or(inner_ascent)
            .max(Au::zero());

        let resolved_descent = self
            .resolve_length(&self.depth, inner_descent)
            .unwrap_or(inner_descent)
            .max(Au::zero());

        let lspace = self
            .resolve_length(&self.lspace, Au::zero())
            .unwrap_or_default();

        let voffset = self
            .resolve_length(&self.voffset, Au::zero())
            .unwrap_or_default();

        // 4. Calculate positioning offsets
        let offset_x = if is_rtl {
            resolved_width - inner_width - lspace
        } else {
            lspace
        };

        // Positive voffset moves content upwards in physical space
        let offset_y = -voffset;

        let physical_offset = PhysicalVec::new(
            offset_x + physical_padding.left,
            offset_y + physical_padding.top,
        );

        translate_fragments(&mut child_fragments, physical_offset.x, physical_offset.y);

        // 5. Construct container fragment geometry
        let content_size = LogicalVec2 {
            inline: resolved_width,
            block: resolved_ascent + resolved_descent,
        };

        let content_rect = LogicalRect {
            start_corner: LogicalVec2::zero(),
            size: content_size,
        }
        .as_physical(Some(containing_block));

        let box_fragment = BoxFragment::new(
            self.base.base_fragment_info.clone(),
            self.base.style.clone(),
            child_fragments,
            content_rect,
            physical_padding,
            pbm.border.to_physical(writing_mode),
            pbm.margin.auto_is(Au::zero).to_physical(writing_mode),
            None,
        );

        vec![Fragment::Box(box_fragment.into())]
    }
}
