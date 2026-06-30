# Sizing & Constraint System Audit

Scope: read-only audit of `src/layout/{engine,block,inline,table,flex,grid,multicol,images,text,math,helpers,context}.rs`, sizing-relevant style resolution, and render geometry consumers at checkout `74cef565` on branch `sizing-audit`.

Probe artifacts were written under `/tmp/ip-sizeaudit-probes`. Ironpress was rendered with `/home/frederic/IdeaProjects/ironpress/target/release/ironpress`. Chromium was rendered through `/snap/bin/chromium`; because the snap cannot read/write host subdirectories under `/tmp` through `--print-to-pdf`, the probes used data URLs for DOM metrics and a local Chrome DevTools Protocol printer for page-count PDFs written by the host process under `/tmp`.

## Part A - Sizing Map

### Shared Constraint Plumbing

| Area | Current code | Behavior | Divergence |
|---|---:|---|---|
| Layout context width | `src/layout/context.rs:45-60`, `src/layout/context.rs:85-88` | `available_width()` is `parent.content_width`; `percent_width_basis` is a second field on `ParentBox`. | Only some callers preserve the distinction. Style resolution ignores it. |
| Parent context construction | `src/layout/context.rs:95-136` | `with_parent()` sets available width and percentage basis to the same value; `with_parent_and_basis()` lets flex split them. | This is an ad hoc constraint object without intrinsic/min/max/definite state. |
| Containing block and percent-height CB | `src/layout/context.rs:138-160` | Absolute containing block and percentage-height containing block are optional side channels. | Many synthetic contexts drop one or both, especially table/grid/inline-block nested layout. |
| Style percentage resolution | `src/style/resolve.rs:13-20`, `src/style/resolve.rs:66-75`, `src/style/resolve.rs:148-181` | `LengthResolutionContext` has one `parent_width`; `%` resolves against it. `From<&LayoutContext>` uses `ctx.parent.content_width`, not `percent_width_basis`. | Style-time percentages and layout-time percentages can resolve against different bases. Height percentages reuse the width field by convention. |
| Deferred percentages | `src/style/computed.rs:1558-1565`, `src/style/computed.rs:3675-3690`, `src/style/computed.rs:5983-6035`, `src/style/computed.rs:6043-6148` | Width/min/max percentages are sometimes deferred into `percentage_sizing`; heights defer unless parent style has a definite height. | Definite state is inferred from `parent.width`/`parent.height` on `ComputedStyle`, not from the actual layout constraint. |

### Available / Containing Width Propagation

| Container / child kind | Where width is chosen or passed | Current behavior | Matches canonical block algorithm? |
|---|---:|---|---|
| Root block children | `src/layout/engine.rs:1794-1808` | Root `LayoutContext` has page content width and content height. | Mostly yes for page root. |
| Generic block | `src/layout/block.rs:319-397`, `src/layout/block.rs:569-574` | Block used width starts from `ctx.available_width()`, then explicit/percent/intrinsic paths adjust it. Child content width is `inner_width`. | Canonical-ish, but min/max/box-sizing/ratio order is local and not reused. |
| Block nested children | `src/layout/block.rs:1365`, `src/layout/block.rs:2269-2278`, `src/layout/block.rs:2741-2749` | Children get `ctx.with_parent(inner_width, Some(available_height), ...)`; percent-height CB is separately patched only in some branches. | Fork. Width is usually correct; height basis is not a unified constraint. |
| Table element from normal flow | `src/layout/engine.rs:4785-4797` | `flatten_table(..., available_width, ...)` receives only an `f32`, not a full `LayoutContext`. | Fork. Table cannot preserve percent basis, block basis, or abs CB. |
| Table inner width | `src/layout/table.rs:1200-1213`, `src/layout/table.rs:1533-1548` | Table width is `style.width`/percentage or available width minus margins, clamped to containing width. | Fork. The table path has its own used-width algorithm. |
| Table cell sizing pass | `src/layout/table.rs:2148-2162` | Cell content is collected with `available_width = inner_width.max(1.0)`, i.e. the whole table inner width. | **Bug source.** Nested containers in a cell are measured against the table, not the cell. |
| Table cell final layout pass | `src/layout/table.rs:2797-2800`, `src/layout/table.rs:2838-2852` | Final cell content is collected with `available_width = cell_inner.max(1.0)`. | Final layout is closer to correct than the sizing pass, so measurement and layout diverge. |
| Table nested synthetic context | `src/layout/table.rs:3613-3627` | Nested block/table/image layout is run in a freshly constructed `LayoutContext` with viewport width and parent width set to `available_width`; no abs CB and no percent-height CB. | Fork. This is the direct fragmentation point for nested table/height bugs. |
| Flex container | `src/layout/flex.rs:650-697` | Flex container resolves its own width, then `inner_width`. | Fork, local min/max/box sizing. |
| Flex item style basis | `src/layout/flex.rs:1247-1277`, `src/layout/flex.rs:1386-1390` | Child style percentages use `width_for_percentages = inner_width - gaps`; flex-basis percentages use `inner_width`. | Divergent within flex itself. |
| Flex nested item layout | `src/layout/flex.rs:1518-1526`, `src/layout/flex.rs:1582-1589` | Probe layout and real nested layout use `with_parent_and_basis`; fake height `Some(10000.0)` appears in the probe path. | Fork. It is a local intrinsic/constraint surrogate. |
| Grid container | `src/layout/grid.rs:2095-2123` | Grid computes `inner_width` from style width or available width minus padding/margins. | Fork, local algorithm. |
| Grid item final children | `src/layout/grid.rs:1280-1284`, `src/layout/grid.rs:2923-2939` | Block children inside a grid item get item content width and optional content height through `with_parent()`. | Width mostly correct; percentage-height CB is not established. |
| Grid abspos children | `src/layout/grid.rs:3054-3149` | Abspos grid children get a padding-box CB and context basis equal to CB width. | Mostly correct, but independent of block abspos. |
| Inline-block / inline-table | `src/layout/inline.rs:225-233`, `src/layout/inline.rs:573-587`, `src/layout/inline.rs:622-668` | Atomic inline children are flattened under context-specific available widths; inline-table uses `ctx.available_width()`. | Fork. Inline-block has its own shrink-to-fit and nested width estimates. |
| Replaced elements | `src/layout/engine.rs:4312-4322`, `src/layout/images.rs:140-145`, `src/layout/images.rs:630-660` | Images receive only available width/height plus style, then local constraint code scales. | Fork. No unified min/max/aspect/box-sizing order. |
| Multicol children | `src/layout/multicol.rs:174-228`, `src/layout/multicol.rs:264-280` | Multicol resolves column width locally, then lays children under column or full-width contexts. | Fork. No shared intrinsic sizing for column width/count. |
| Abspos | `src/layout/block.rs:653-703`, `src/layout/block.rs:2598-2683`, `src/layout/helpers.rs:2590-2634` | Containing blocks are made per container; bottom/right resolution is helper-based. | Partially shared, but only for offsets, not used size. |

**Nested-table root cause:** table auto sizing measures nested cell content at `src/layout/table.rs:2148-2162` with the whole table `inner_width`; final layout later uses `cell_inner` at `src/layout/table.rs:2838-2852`. The synthetic nested context at `src/layout/table.rs:3613-3627` makes that passed width both `available_width` and `percent_width_basis`. A nested `table { width:100%; table-layout:fixed }` inside a cell can therefore contribute intrinsic/preferred widths as if it lived in the whole table, then lay out under a different width.

### Width Resolution

| Container | Where computed | How it works now | Divergence |
|---|---:|---|---|
| Block used width | `src/layout/block.rs:331-463` | Starts from available width, handles explicit width, deferred percent width, intrinsic keywords, aspect-ratio-from-height, min/max. | Local fork. Percentage basis can differ from style resolution; min/max order is local. |
| Table fixed layout | `src/layout/table.rs:1246-1404`, `src/layout/table.rs:1967-1993` | Colgroup/col widths and first-row cell widths resolve against table width; unresolved columns split remaining width. | Correct domain algorithm, but no shared child measurement. |
| Table auto layout | `src/layout/table.rs:1995-2372` | Computes per-column preferred/min widths from text, explicit widths, and `nested_element_preferred_width()`. | Fork. Child intrinsic sizes are reimplemented from laid-out boxes and can use the wrong containing width. |
| Table cell content width | `src/layout/table.rs:2785-2800` | Final `cell_inner` is span width minus padding/border. | Correct final basis, but not used by the sizing pass. |
| Flex basis/grow/shrink | `src/layout/flex.rs:1180-1210`, `src/layout/flex.rs:1247-1603`, `src/layout/flex.rs:1726-1906` | Flex has local basis, auto-min, text measurement, probe layout, and child relayout paths. | Fork. It cannot ask any child for a uniform min/max-content contribution. |
| Grid tracks | `src/layout/grid.rs:102-344`, `src/layout/grid.rs:2498-2533` | Tracks use min/max intrinsic arrays populated by grid-specific text measurement. | Fork. Direct replaced items and nested block children are not measured through the real child layout contract. |
| Inline-block shrink-to-fit | `src/layout/inline.rs:622-668` | Width is explicit `child_style.width` or max line width; nested widths are inferred from already flattened elements. | Fork. `width:min-content` is ignored here. |
| Replaced intrinsic width | `src/layout/images.rs:208-244`, `src/layout/images.rs:630-660` | CSS/HTML width/height or natural dimensions are chosen, then scaled down by available width, max-width, max-height. | Fork. Scaling treats max-height as an object-fit-like scale even when CSS would clamp the used height after a specified width. |
| Multicol columns | `src/layout/multicol.rs:180-228` | Inner width and column count/width are resolved locally. | Fork. Column width selection is independent of shared block/table/flex/grid sizing. |
| Abspos width | `src/layout/block.rs:447-463`, `src/layout/helpers.rs:2590-2634` | Block stretch width and right/left offsets use local containing block widths. | Partial fork. Offset helper exists; used-size resolver does not. |

### Height Resolution

| Container | Where computed | How it works now | Divergence |
|---|---:|---|---|
| Block height | `src/layout/block.rs:467-510`, `src/layout/block.rs:2410-2453`, `src/layout/helpers.rs:519-561` | Explicit/percent height is handled early; auto height later uses content, min-height, aspect-ratio, and max-height clamp. | Fork. `max-height` only clamps definite height early; auto max-height is a later block-wrapper path. |
| Percentage height | `src/layout/block.rs:484-497`, `src/layout/block.rs:620-645`, `src/layout/block.rs:2257-2268` | Percent heights resolve against abs CB or `percent_height_cb`, which only some parents install. | Divergent. A parent content height in `LayoutContext` is not enough. |
| Table row height | `src/layout/table.rs:878-931`, `src/layout/table.rs:2880-2927`, `src/layout/table.rs:2967-2968`, `src/layout/table.rs:3051-3062` | Rows use cell content heights, explicit row/cell heights, row spans, and table min-height stretching. | Fork. Cell nested content height is derived from locally flattened child boxes. |
| Flex main/cross height | `src/layout/flex.rs:1204-1243`, `src/layout/flex.rs:1846-1870`, `src/layout/flex.rs:2257`, `src/layout/flex.rs:3320-3353` | Flex computes item min/max cross/main constraints in closures and later relayouts items. | Fork. It has its own min/max/aspect logic. |
| Grid row height | `src/layout/grid.rs:346-354`, `src/layout/grid.rs:1179-1246`, `src/layout/grid.rs:2535-2631` | Fixed/percent row tracks are handled locally; auto rows grow from `grid_item_outer_height()`. | Fork. `grid_item_outer_height()` ignores aspect-ratio and direct replaced items. |
| Inline-block height | `src/layout/inline.rs:654-668`, `src/layout/inline.rs:472-621` | Inline-block height is explicit or sum of wrapped lines plus nested element estimates. | Fork. Intrinsic and used height are coupled to the inline atomic path. |
| Replaced height | `src/layout/images.rs:221-244`, `src/layout/images.rs:630-660` | Natural/CSS/HTML height then scale by max-height. | Fork. The tall-image probe shows CSS used-height behavior is not matched when width is definite. |
| Multicol height | `src/layout/multicol.rs:328-338`, `src/layout/multicol.rs:383-688` | Explicit border-box height, page-aware column fill, and balanced column heights are computed locally. | Domain-specific fork. |

### Intrinsic Sizing

| Source | Where | What it can measure | Gap |
|---|---:|---|---|
| Generic helper | `src/layout/helpers.rs:791-864`, `src/layout/helpers.rs:866-964`, `src/layout/helpers.rs:1053-1081` | Recursive text/block intrinsic widths for some block-like descendants; supports `min-content`, `max-content`, `fit-content`. | Not a universal child contract. Tables/flex/grid/replaced are not delegated to their own intrinsic algorithms. |
| Table intrinsic/min/max | `src/layout/table.rs:2164-2276`, `src/layout/table.rs:2226-2233` | Manual text longest-word/max-line and nested preferred width from laid-out elements. | Reimplements intrinsic sizing and can lay out children under wrong width. |
| Flex intrinsic | `src/layout/flex.rs:30-77`, `src/layout/flex.rs:79-125`, `src/layout/flex.rs:474-570` | Probe extents, text min-content, and flex container width keywords. | Reimplements child contributions and misses arbitrary child semantics. |
| Grid intrinsic | `src/layout/grid.rs:1090-1124`, `src/layout/grid.rs:2498-2525` | Text runs only; if a grid item has a block child it measures leading inline runs before the block. | Direct replaced grid items and nested block-only content can measure as zero. |
| Inline-block intrinsic | `src/layout/inline.rs:124-156`, `src/layout/inline.rs:622-668` | Derives nested outer width from flattened output and line max width. | Does not use `width_keyword`; shrink-to-fit is not CSS `min(max-content, max(min-content, available))`. |
| Replaced intrinsic | `src/layout/images.rs:221-235`, `src/layout/images.rs:898-940` | Natural raster/SVG dimensions and SVG percentage dimension handling. | Not exposed as `measure_intrinsic()` to table/flex/grid/inline-block. |

### Constraint Propagation And Clamp Order

| Rule | Current state | Consequence |
|---|---|---|
| Box sizing conversion | Block: `src/layout/block.rs:334-342`, `src/layout/block.rs:435-445`; grid: `src/layout/grid.rs:2100-2123`; multicol: `src/layout/multicol.rs:174-192`; helpers: `src/layout/helpers.rs:519-561`; images: `src/layout/images.rs:251-253`. | Same conversion is copied in several forms. Border-box vs content-box decisions can drift by container. |
| Percent width basis | Deferred width percentages use `percent_width_basis` in block (`src/layout/block.rs:344-390`), flex manually splits bases (`src/layout/flex.rs:1247-1277`), table synthetic contexts set basis to their `available_width` (`src/layout/table.rs:3613-3627`), grid does not re-resolve `percentage_sizing.width` for items (`src/layout/grid.rs:1499-1577`). | Percent width is container-specific. The grid item probe resolves `width:50%` to 300pt in Iron instead of Chromium's 150pt. |
| Percent height basis | Style defers height percentages unless parent style height is definite (`src/style/computed.rs:6043-6148`); block layout needs `percent_height_cb` (`src/layout/block.rs:484-497`); grid item children only receive `content_height` through `with_parent()` (`src/layout/grid.rs:1280-1284`). | Grid child `height:50%` inside a 100pt grid area disappears in Iron. |
| Aspect-ratio | Block helper `aspect_ratio_height()` only derives height from width (`src/layout/helpers.rs:2501-2507`); block uses it in wrapper height (`src/layout/block.rs:2429-2432`); flex has local transfer points (`src/layout/flex.rs:535`, `src/layout/flex.rs:1420`, `src/layout/flex.rs:1850`); grid item height ignores it; images use scale-down. | Aspect-ratio is not part of a shared used-size resolver. Grid aspect boxes vanish; tall image max-height changes width. |
| Min/max order | Block applies width max before min (`src/layout/block.rs:398-433`) and height max differently for definite vs auto (`src/layout/block.rs:505-510`, `src/layout/block.rs:2434-2453`); flex has `main_min_max` / `cross_min_max` closures (`src/layout/flex.rs:1180-1243`); images scale by min of available/max dimensions (`src/layout/images.rs:630-660`). | CSS Sizing min/max transfer and aspect-ratio clamping are not consistently implemented. |
| Render fallback | `src/render/pdf.rs:2140-2207`, `src/render/pdf.rs:2210-2290` | Render uses layout-supplied `block_width`/`block_height` when present, otherwise falls back to content/padding sizes. | Paint is mostly a consumer. It can amplify missing layout sizes, but it is not the root cause of the audited bugs. |

## Part B - Probe Matrix

The probe runner is `/tmp/ip-sizeaudit-probes/run_sizing_probes.py`; detailed JSON is `/tmp/ip-sizeaudit-probes/results.json`. Chromium DOM boxes are the numeric reference below because the Chromium PDF print pipeline globally scales colored content even when CSS layout is correct. The real certificate repro used CDP PDF page counts.

| Case | Chromium reference | Ironpress observation | Verdict | Divergent path |
|---|---:|---:|---|---|
| `/tmp/cert-groundtruth.html` real repro | Chromium CDP PDF: 1 page | Ironpress PDF: 2 pages | BUG | Nested `.author-details { width:100%; table-layout:fixed }` tables in `.author-entry__cell`; source root is table sizing pass using table `inner_width` at `src/layout/table.rs:2148-2162`, then final pass using `cell_inner` at `src/layout/table.rs:2838-2852`. |
| Minimal fixed nested table in cell | Nested table 220pt; fixed cols 154pt / 66pt | 154pt / 66pt | OK | Final table-cell pass is correct for this simplified fixed case. It isolates the real bug to auto/intrinsic table measurement, not paint. |
| `width:100%` in table cell | 150pt | 150pt | OK | Final `cell_inner` path works for simple block child. |
| `width:50%` flex item with gap | 150pt | 150pt | OK | This probe did not expose the flex basis/style-basis split. Source still has divergent bases at `src/layout/flex.rs:1247-1277` and `src/layout/flex.rs:1386-1390`. |
| `width:50%` grid item | 150pt | 300pt | BUG | Grid computes child styles before track width and `compute_grid_inset()` uses `cs.width` only, ignoring `percentage_sizing.width` (`src/layout/grid.rs:1499-1577`). |
| `width:100%` in inline-block with explicit width | 150pt | 150pt | OK | Explicit inline-block width lets style resolve against parent style width. |
| `height:50%` in table cell with 100pt height | 50pt | 50pt | OK | Cell final pass installs a parent style height via `cell_content_style.height` (`src/layout/table.rs:2800-2815`). |
| `height:50%` child inside 100pt grid row | 50pt | missing / 0pt | BUG | `layout_grid_item_children()` passes `content_height` to `with_parent()` but does not install `percent_height_cb`; block percent height cannot resolve (`src/layout/grid.rs:1280-1284`, `src/layout/block.rs:484-497`). |
| `width:80pt; aspect-ratio:2/1` in table cell | 80pt x 40pt | 80pt x 40pt | OK | Block aspect-ratio wrapper path handles this case. |
| `width:80pt; aspect-ratio:2/1` as grid item | 80pt x 40pt | missing / 0pt | BUG | `grid_item_outer_height()` ignores aspect ratio and direct item boxes (`src/layout/grid.rs:1179-1246`). |
| Flex item `height:120pt; max-height:40pt` | 80pt x 40pt | 80pt x 40pt | OK | Flex local cross max path clamps this simple case. |
| Tall image in table cell, `width:30pt; max-height:60pt` | DOM used box 30pt x 60pt | Red image 6pt x 60pt | BUG | `constrain_replaced_image_size()` scales both axes by `max-height / height` (`src/layout/images.rs:630-660`) instead of using a shared CSS used-size resolver. This is the tall aspect-ratio image failure mode. |
| Grid `min-content` track containing nested block text | 172.1pt | missing / 0pt | BUG | `grid_item_intrinsic_widths()` measures only text runs/leading runs and does not ask the nested block for intrinsic width (`src/layout/grid.rs:1090-1124`). |
| Inline-block `width:min-content` | 96.7pt wide, 27pt tall | 127pt wide, 13.5pt tall | BUG | Inline-block ignores `width_keyword` and uses max line width (`src/layout/inline.rs:622-668`). |
| Intrinsic SVG image in block/table/flex/inline-block | 60pt x 30pt | 60pt x 30pt | OK | Replaced intrinsic works in these contexts. |
| Intrinsic SVG image as direct grid item | 60pt x 30pt | missing | BUG | Direct replaced grid items are not laid out through `flatten_element()` or a replaced intrinsic contract; grid rows are table-cell text/nested-block approximations. |
| `box-sizing:border-box; width:100%; padding:10pt` nested in table/flex/grid/inline-block | 160pt in all four contexts | 160pt in all four contexts | OK | Basic border-box percentage width survives these simple nesting cases. |

## Part C - Unified Constraint / Sizing Model

The missing abstraction is not another container-specific helper. It is a single constraint space plus two universal contracts:

```rust
#[derive(Clone, Copy, Debug)]
enum AvailableSize {
    Definite(f32),
    MinContent,
    MaxContent,
    FitContent(f32),
    Stretch,
    Indefinite,
}

#[derive(Clone, Copy, Debug)]
struct AxisConstraint {
    available: AvailableSize,
    percent_basis: Option<f32>,
    min: Option<AvailableSize>,
    max: Option<AvailableSize>,
}

#[derive(Clone, Copy, Debug)]
struct ConstraintSpace {
    inline: AxisConstraint,
    block: AxisConstraint,
    containing_block_inline: Option<f32>,
    containing_block_block: Option<f32>,
    abs_containing_block: Option<ContainingBlock>,
    writing_mode: WritingMode,
}

#[derive(Clone, Copy, Debug)]
struct IntrinsicSizes {
    min_content: f32,
    max_content: f32,
}

#[derive(Clone, Copy, Debug)]
struct UsedBox {
    content_inline: f32,
    content_block: f32,
    border_inline: f32,
    border_block: f32,
}
```

Required contracts:

```rust
fn measure_intrinsic(node: LayoutNodeRef, env: &mut LayoutEnv, axis: Axis) -> IntrinsicSizes;

fn resolve_used_size(
    node: LayoutNodeRef,
    style: &ComputedStyle,
    constraints: ConstraintSpace,
    intrinsic: Option<IntrinsicSizes>,
    env: &mut LayoutEnv,
) -> UsedBox;
```

The resolver owns the CSS Sizing-3/4 order:

1. Convert specified `width`/`height`/`min-*`/`max-*` through box-sizing into a content-box preferred size, retaining border-box output.
2. Resolve percentages only against `AxisConstraint.percent_basis`; if absent, keep the value indefinite and fall back to auto/intrinsic rules.
3. If an intrinsic keyword is specified, call `measure_intrinsic()` and apply `min-content`, `max-content`, or `fit-content = min(max-content, max(min-content, stretch-fit))`.
4. Apply replaced-element natural dimensions and preferred aspect ratio through the same path as CSS `aspect-ratio`.
5. Apply min/max clamps and aspect-ratio transfer in one place, including the CSS rule that min/max constraints on one axis can affect the other axis through the preferred ratio when that axis is auto.
6. Return content-box and border-box sizes so layout, pagination, and render never recompute padding/border math.

How every container reduces to the two contracts:

| Container | Unified behavior |
|---|---|
| Block | `layout_block_element` creates a `ConstraintSpace` from the parent content box and calls `resolve_used_size()` for the block. Children receive a new constraint whose inline available/percent basis is the block content width and whose block percent basis is the content height only when definite. |
| Table fixed layout | Table width is resolved by `resolve_used_size()`. Column/colgroup percentages resolve against the table grid width per CSS2 17.5.2. Fixed cells then lay out children with `ConstraintSpace.inline.percent_basis = cell_content_width`. |
| Table auto layout | Cell min/max contributions call `measure_intrinsic(child)` for any child kind. A nested table contributes its min/max table width without being flattened under the outer table width. Final layout calls `resolve_used_size(child, cell_constraint)`. |
| Flex | `flex-basis:auto/content/min-content/max-content/fit-content` becomes `resolve_used_size()` or `measure_intrinsic()` under the flex container's definite inner main size. Grow/shrink operates on resolved flex base sizes; auto min-size uses `measure_intrinsic()` for all child kinds. |
| Grid | Track min/max-content arrays are filled by `measure_intrinsic(grid_item)`, not by text-run-only logic. Final grid item size uses the grid area as both available size and percentage basis; children inherit a definite block percent basis when the row span is definite. |
| Inline-block / floats | Shrink-to-fit is exactly `min(max-content, max(min-content, available))` from `measure_intrinsic()`, then children are laid out once under the resolved content width. |
| Replaced elements | Natural raster/SVG dimensions produce intrinsic min/max and preferred ratio. CSS width/height/min/max/box-sizing/aspect-ratio use the same resolver as blocks. |
| Abspos | The absolute containing block's padding box becomes `ConstraintSpace.containing_block_*` and the same resolver handles auto/left/right/stretch sizes; `resolve_abs_containing_block()` remains an offset helper, not a size algorithm. |
| Multicol | Column measure/layout uses a child `ConstraintSpace` for the column content width. Later phases can use `measure_intrinsic()` for `column-width:auto` decisions and span-all bands. |

Why the two reported bugs disappear:

- Nested table in cell: the nested table no longer sees the outer table `inner_width` during measurement. The table cell creates a cell constraint, so `width:100%` on the nested table resolves against the cell content width in final layout, and intrinsic auto-table contributions are obtained through `measure_intrinsic()` rather than through a wrong-width flatten pass.
- Tall image in cell: the image has one used-size path. With `width:30pt; max-height:60pt`, the resolver sees a definite inline size, derives the auto block size from the intrinsic ratio, then applies `max-height` in the CSS-defined clamp/ratio-transfer order. Table row height consumes that returned `UsedBox`; no table or image path rescales it independently.

## Part D - Phased Migration Plan

### Phase 0 - Lock Evidence

- Preserve the `/tmp/ip-sizeaudit-probes` cases as future parity fixtures once the implementation branch is ready. Do not add them in this audit-only change.
- Add tracing around child constraint creation in table/flex/grid only during implementation, not in render.
- Keep the existing parity gate green; add new deep-nesting fixtures one at a time so failures identify the container that still bypasses the shared resolver.

### Phase 1 - Smallest Fix For Nested Tables And Tall Images

Goal: fix the known nested-table-in-cell and tall-image bugs without rewriting flex/grid.

1. Introduce a small internal `ChildConstraint` / `ResolvedBox` helper near the existing layout helpers, limited to inline/block available size, percent basis, box-sizing conversion, min/max, and aspect-ratio for blocks/replaced elements.
2. Change `collect_table_cell_content_inner()` so callers pass a cell constraint, not a bare `available_width`.
   - In final row layout, use `cell_inner` as both available inline size and percentage basis.
   - When the cell has a definite content height, install the block percentage basis / `percent_height_cb` instead of leaving it `None`.
   - Remove the synthetic viewport-width context at `src/layout/table.rs:3613-3627` as the source of truth; derive it from the cell constraint.
3. In the table sizing pass, stop measuring nested block/table/image children by flattening them against `inner_width` at `src/layout/table.rs:2148-2162`.
   - For the smallest safe change, route nested table/image/block contributions through a `measure_child_intrinsic_for_table_cell()` wrapper.
   - Treat percentage widths as indefinite during intrinsic contribution unless the cell/span width is already definite.
   - Keep fixed colgroup percentage resolution against the nested table's own used table width, not the outer table width.
4. Replace `constrain_replaced_image_size()` for HTML/SVG images with the same helper's replaced-size path.
   - Preserve natural raster/SVG intrinsic loading.
   - Apply `max-height` through the used-size clamp path so specified width is not unintentionally scaled from 30pt to 6pt.
5. Add two implementation tests first: the real certificate should remain 1 page against Chromium; the tall table-cell image should match Chromium's used box/row height.

This phase deliberately avoids flex/grid rewrites. It converges the table-cell nested-container path and replaced sizing path enough to eliminate the current production failures.

### Phase 2 - Block And Inline-Block Unification

- Move block width/height/min/max/aspect code from `src/layout/block.rs:331-510` and `src/layout/block.rs:2410-2453` into `resolve_used_size()`.
- Make `resolve_intrinsic_keyword_width()` a thin adapter over `measure_intrinsic()`.
- Replace inline-block width selection in `src/layout/inline.rs:622-668` with shrink-to-fit from `measure_intrinsic()`, fixing `width:min-content` and nested block/replaced contributions.

### Phase 3 - Table Intrinsic Sizing

- Replace table auto-layout cell text/nested width accumulation at `src/layout/table.rs:2164-2276` with calls to `measure_intrinsic()` for each cell child.
- Keep CSS2 table-specific distribution in table code; only the child contribution source becomes shared.
- Ensure column percentage widths still resolve against the table grid width (`src/layout/table.rs:595-601`, `src/layout/table.rs:335-340`).

### Phase 4 - Flex Basis, Auto Min-Size, And Relayout

- Replace `flex_probe_outer_extent()` and `flex_text_min_content()` with `measure_intrinsic()` (`src/layout/flex.rs:30-125`).
- Resolve `flex-basis`, `width`, min/max, and aspect-ratio through `resolve_used_size()` before grow/shrink.
- Make gap subtraction affect free-space distribution only, not the percentage basis for item widths.

### Phase 5 - Grid Track And Item Sizing

- Fill grid min/max intrinsic arrays from `measure_intrinsic()` instead of `grid_item_intrinsic_widths()` (`src/layout/grid.rs:1090-1124`, `src/layout/grid.rs:2498-2525`).
- Resolve grid item percentages against the grid area after track sizing, fixing `width:50%` and direct replaced grid items.
- Establish a definite block percentage basis for children in definite row spans, fixing `height:50%` inside grid items.
- Include aspect-ratio in `grid_item_outer_height()`.

### Phase 6 - Abspos, Multicol, And Render Cleanup

- Move abspos stretch size into `resolve_used_size()` while keeping offset resolution in `resolve_abs_containing_block()`.
- Let multicol build column child constraints from the shared type, then later use intrinsic measurement for column-width decisions.
- Audit render fallbacks in `src/render/pdf.rs:2140-2290`; after layout supplies consistent `UsedBox` sizes, render should not infer layout dimensions except for text painting internals.

### Risk Areas

- Table auto layout is the highest risk: CSS2 table width distribution is legitimately special, so only child intrinsic contributions should be unified first.
- Flex and grid have many parity patches. Keep their distribution algorithms local while replacing child measurement and used-size resolution underneath.
- Percentage-height behavior depends on definiteness. The new constraint type must distinguish `None`/indefinite from `Some(0)` and from `Definite(0)`.
- Replaced SVG/raster sizing must preserve existing image loading, DPI, SVG viewBox, and percentage SVG attribute handling while changing only CSS used-size resolution.
- Pagination must consume returned border-box/block sizes; otherwise row/page breaks can still diverge after used sizes are fixed.
