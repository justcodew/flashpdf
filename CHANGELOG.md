# Changelog

## [0.4.0] - 2026-06-27

Phase 1 完成 —— fitz 功能补全。新增 5 个 API 表面，向后兼容 0.3.x。

### Added

- **`doc.metadata`** (1.1)：返回 fitz 兼容的 metadata dict
  （title/author/subject/keywords/creator/producer/creationDate/modDate/
  format/encryption/size）。UTF-16BE CJK + PDFDocEncoding + hex string
  + literal escape 都正确解码。`doc.pdf_version` 暴露 `%PDF-X.Y`。

- **`page.get_links()` + `extract_links()`** (1.2)：多类型链接提取
  （Uri/Goto/Named/Launch/GotoR）。Link annot 的 `/A` action 和 `/Dest`
  都覆盖；dest 数组 `/XYZ` 解析为目标页+点。corpus 146/150 与 fitz 一致
  （4 个差异为 ObjStm 压缩 annot 引用，pre-existing 限制）。

- **`span["flags"]` 格式探测** (1.3)：fitz bitmask
  （italic=2, serif=4, mono=8, bold=16）。从 `/FontDescriptor /Flags`
  + 名称启发式推断（Times/Courier/NimbusRom/NimbusMono/CM* 等）。
  **限制**：当前是页级字体选择（per-page font），所有 span 继承同一
  字体的 flags；per-span 需要把 `font_map` 下沉到 `cluster_chars`，
  留到后续版本。

- **`doc.get_toc()` outline 提取** (1.4)：DFS 遍历 `/Outlines` 字典树，
  周期安全（visited-set）。Title 支持间接引用 + UTF-16BE。Named dest
  通过 `/Names /Dests` Name Tree（PDF §7.9.6）解析为目标页；显式数组
  dest 的 `/XYZ` 解析为 `to_point`。
  - `get_toc(simple=True)` → `[[level, title, page], ...]`（1-based 页码）
  - `get_toc(simple=False)` → 富 dict（kind/uri/to_point/name）

- **`flashpdf` CLI** (1.5)：基于 click，pip 安装自动注册。
  - `flashpdf extract <pdf...>` — text/dict/blocks 模式，`--pages 0,1,5-8`
    子集，`--output-dir` 批量，Windows glob 展开
  - `flashpdf info <pdf>` — JSON 元数据 + 页统计
  - `flashpdf toc <pdf>` — 树状缩进或富 JSON

### Verification

- 77/77 Rust 核心 unit tests 过；11/11 CLI smoke tests 过
- PyMuPDF 165-PDF bug-regression corpus：0% 失败率，速度 vs 0.3.2 无回退
- TOC vs fitz 在 `2604.11578v1.pdf` 上 25/25 完全一致（level/title/page）
- `cargo fmt --all --check && cargo clippy -- -D warnings` 干净

### Breaking Changes

无。所有新 API 与 `extract()` / `extract_many()` / `open()` 并行加入。

---

## [0.3.2] - 2026-06-26

### Fixed

- **`xy_cut` 无限递归（SIGBUS on `test_3072.pdf`）**：`largest_gap` 计算 gap
  时过滤掉 extent > 70% 的"宽"块（页码横幅、整页侧栏），但 `split_by_axis`
  对所有块做切分。当 gap 只存在于过滤后的窄块子集中时，split 一边为空、
  另一边是全集 → 递归 `xy_cut(全集)` 找同一个 gap 同一个 split → 无限递归
  → 栈溢出 → SIGBUS（exit 138）。

  加防重入守卫：split 后任一半为空就 re-merge 并 fall through 到下一个
  cut 或 sort fallback，而不是带着空半 recurse。

- **recovery 记录错位的对象偏移**：`recover_xref_by_scan` 之前把
  `"obj"` 关键字的字节位置当作对象偏移记进 xref，但 `parse_object_at`
  期望偏移指向对象头部的起始数字（`N G obj`）。导致每个 recovery-built
  xref 条目都让 `parse_object_at` 在 `"obj\n<<..."` 上读不到数字，
  InvalidNumber 错误，所有页都识别不到。

  `try_parse_obj_header` 返回 `(obj_num, gen, header_start)` 三元组，
  其中 `header_start` 是对象号起始数字的字节位置，记进 xref。

- **`from_mmap` 的 `?` 短路绕过 recovery fallback**：xref-stream 分支
  里 `resolve_indirect_object_raw(...)?` 直接从函数返回，导致下游的
  `match xref { Err(_) => recover_xref_by_scan(...)? }` 永远看不到这个
  错误。重构为 `parse_xref_at` helper 返回 `Result<XrefTable>`，所有
  错误统一走 recovery fallback。

- **xref stream 未解压**：`parse_xref_at` 把 raw（仍压缩的）stream 字节
  直接交给 `parse_xref_stream_obj`，导致 "xref stream data too short"。
  xref stream 几乎总是 FlateDecode 压缩，现在先按 `/Filter` 解压再解析。

- **ObjStm 内对象边界计算错误**：`parse_objstm` 第 i 个对象的 start 用了
  `obj_offsets[i-1]`（前一个对象的起点）而非 `obj_offsets[i]`（自己的
  起点）。i > 0 的每个对象都把前一个对象的字节当作自己的内容来 parse，
  结果 ObjStm 里只有第 0 个对象正确。`cython.pdf` 的 catalog（176）藏在
  ObjStm 139 里因此完全取不到，page_refs 失败。

- **xref root 偏移有效性校验**：`xref_root_ok` 新增——xref 解析成功后，
  校验 root 对象的偏移确实指向 `"N G obj"` 头部。校验失败则 fall through
  到 recovery scan。捕获 test2238.pdf（120 字节 prefix garbage 导致所有
  xref 偏移整体偏移）这类文件。

### Benchmark 影响

PyMuPDF bug-regression corpus（165 个病理 PDF）：

| 指标 | v0.3.1 | v0.3.2 |
|------|-------:|-------:|
| flashpdf 失败率 | 2% (4/165) | **0% (0/165)** |
| CRASH (test_3072) | 1 | **0** |
| ValueError (test2238/2788/cython) | 3 | **0** |
| geo-mean vs liteparse | 2.78× | **2.12×** |
| geo-mean vs pdf_oxide | 2.54× | **1.98×** |

flashpdf 在该 corpus 上失败率最低（liteparse / pdf_oxide 各 1%），速度
geo-mean 仍稳定 2× 同侪。速度数字相比 v0.3.1 略降是因为 v0.3.1 跳过了
test_3072 这个崩溃文件，v0.3.2 把它纳入计时。

按文件大小分桶 speedup（geo-mean）：

| 桶 | n | fp p50 | lp / fp | po / fp |
|------|--:|--:|--:|--:|
| tiny <10KB | 31 | 0.21ms | 1.34× | 0.80× |
| small 10-100KB | 51 | 0.70ms | 1.53× | 1.46× |
| medium 100KB-1MB | 63 | 1.40ms | 2.91× | 3.15× |
| large >1MB | 20 | 6.27ms | 3.64× | 4.22× |

flashpdf 在 medium / large 文件上 2.9-4.1× 领先；tiny 文件 pdf_oxide
略快（0.85×）—— 这是启动开销主导的区间，flashpdf 的 rayon 线程池设置
成本不被几个 KB 的内容抵消。

## [0.3.1] - 2026-06-26

### Fixed

- **xref recovery 无限循环（recovery.rs `skip_value` no-op bug）**：
  `recover_xref_by_scan` 调用的 `parse_minimal_dict` 在遇到 dict value 是
  `[` 数组 / `(` 字符串 / `<` hex string / `<<` 嵌套 dict 时调用 `skip_value`
  跳过，但 `skip_value` 是个 `let _ = data;` 的空函数——`pos` 永远不前进。
  下一次循环的 "skip unknown token" 把 `[` 当成停止字符，循环体一次都不执行，
  形成无限循环。任何 catalog 里有数组字段（/Names /Outlines 等）的 PDF
  open() 阶段就挂死。

  重写 `skip_value` + 5 个 helper（`skip_paren_string` / `skip_hex_string` /
  `skip_dict` / `skip_array` / `skip_name`），返回消费字节数；调用方
  `pos += skip_value(...)`。

  - 修复 PyMuPDF corpus 上 **全部 36 个 open() 阶段 TIMEOUT**（100%）。
  - 这些文件之前 30s SIGKILL，现在大多 < 10ms 完成。

- **悬空间接引用按 null 解析（PDF 1.7 §7.3.10）**：`Document::get_object` 之前对
  xref 中不存在 / Free 条目 / ObjStm 中找不到的对象直接返回 `Err`，导致整个文档
  解析失败。现统一返回 `PdfObject::Null` 并缓存，与 PyMuPDF / pdf_oxide / liteparse
  的 spec-compliant 行为对齐。

  - 触发场景：Word/Office 导出 PDF、增量更新后旧对象被回收、linearized PDF 的
    hint 表里残留引用、AcroForm 字段 /DR 资源悬空。
  - ValueError 失败 46 → 2（95% 修复）。

- **页树断裂恢复**：`Document::recover_page_refs` 新增——当 `/Pages` 或 `/Kids`
  断裂（root 拿不到 Pages dict、Pages dict 拿不到 Kids array），扫所有 xref 条目
  找 `/Type /Page` 对象，按文件字节偏移排序构建页列表。`extract_doc` 在
  `page_refs()` 失败或返回空时自动回退到该路径。

  - 修复 `missing /Pages in catalog` 与 `missing /Kids in Pages` 两类致命错误。
  - 完全断裂的文档现在返回 0 页成功而不是 fatal。

- **空页列表 panic**：`page_refs.chunks(0)` 在 recovery 也返回空时 panic，
  加 `if page_refs.is_empty()` 提前返回空 `ExtractResult`。

### Benchmark 影响

PyMuPDF bug-regression corpus（165 个病理 PDF）：

| 指标 | v0.3.0 | v0.3.1 |
|------|-------:|-------:|
| flashpdf 失败率 | 50% (83/165) | **2% (4/165)** |
| ValueError | 46 | 3 |
| TIMEOUT | 36 | **0** |
| CRASH | 1 | 1 |
| geo-mean vs liteparse | 5.28× | **8.36×** |
| geo-mean vs pdf_oxide | 2.75× | **4.08×** |

flashpdf 失败率已与 peers（liteparse / pdf_oxide 各 1%）同一量级，速度优势
普遍成立——按文件大小分桶 speedup 区间 1.86× ~ 9.62×。
速度提升是因为恢复路径纳入了若干"重"PDF，peers 在这些文件上更慢。

剩下的 4 个失败：3 个 ValueError（含 `cython.pdf` 的 ObjStm 压缩 catalog，
recovery 看不见；2 个 tokenizer-level "invalid number format" / "expected 'obj'
keyword"），1 个 CRASH（`test_3072.pdf`，单一文件）。

## [0.3.0] - 2026-06-26

### Added

- **旋转文本提取（`include_rotated`）**：新增 `ExtractOptions::include_rotated`
  字段，`open()` / `extract()` / `extract_many()` 均新增同名 Python 关键字参数，
  默认 `False`。开启后能正确提取 arXiv 侧栏水印（`arXiv:xxxx [category] date`）
  和图表纵轴标签等通过 `cm`/`Tm` 旋转的文本。

  - 旋转字符通过 TRM = CTM × Tm 的 4-角变换计算 bbox，方向向量驱动 advance
  - 标准 14 字体无 `/Widths` 时，per-char advance 取 0.5em 经验值，
    避免 40 字符侧栏跨出页面边界被阅读序过滤器丢弃
  - 旋转字符独立聚类并**追加到页 block 列表末尾**，不进入 XY-cut 排序，
    正文 char_sim 与 default 行为字节级保持一致

- **`PageDiagnostics`**（4 类检测，**默认开启**，与 `include_rotated` 无关）：
  通过 `page.diagnostics` 暴露 per-page 计数，让用户看到"N 个字符被丢弃"，
  决定是否翻开关重提取或交给 OCR：
  - `rotated_char_count`: 非轴对齐文本矩阵下生成的字符（arXiv 侧栏水印、
    图表纵轴标签）。用户看到 > 0 即可知道用 `include_rotated=True` 重提取。
  - `type3_char_count`: `/Type3` 字体下的字符（字形由绘图算子定义）。
  - `undecoded_byte_count`: 解码失败回退为 `U+FFFD` 的字节数。
  - `out_of_page_block_count`: 被 reading-order 边距过滤器丢弃的块数。

### Changed

- `CharInfo` 新增 `rotated: bool` 字段，标记该字符是否在非轴对齐文本矩阵下
  生成（`Tm` 或 `ctm` 的 b/c 分量非零）。
- `PageResult` 新增 `diagnostics: PageDiagnostics` 字段。
  `extract(..., with_page_info=True)` 的 page 字典也带 `diagnostics`。
- `FontInfo` 新增 `is_type3: bool`，标记 `/Subtype /Type3` 字体；emit 时累计
  `ContentResult.type3_char_count`。
- `ContentResult` 新增 `type3_char_count` / `undecoded_byte_count` 计数器，
  从 `emit_string` 和 Form XObject 递归路径聚合。
- `layout::reading_order_sort_with_diagnostics` 新增——返回 `(Vec, usize)`，
  usize 为边距过滤器丢弃的块数。原 `reading_order_sort` 保留为薄包装。

### Notes

- 检测层和策略层解耦：检测总是发生（diagnostics 在 `include_rotated=False`
  默认模式下也填得满满当当），是否输出由用户开关决定。
- 默认行为字节级保持不变，arxiv_2604 char_sim vs fitz 仍为 0.9360。

## [0.2.0] - 2026-06-24

### Added

- **fitz 风格 API**：新增 `flashpdf.open()` 入口 + `Document` / `Page` 类，与
  PyMuPDF 的常用接口一一对应。解决了 v0.1.x 两个 API 痛点：
  1. 之前 `extract()` 把所有页的 blocks/images 混在扁平列表里，按页处理要
     自己 `groupby(page)`；现在 `doc[i].get_text("dict")` 天然返回 per-page
     数据。
  2. 之前没有 `open()` 入口，与 fitz 用户 muscle memory 不兼容。

  ```python
  import flashpdf
  with flashpdf.open("paper.pdf") as doc:
      page = doc[0]                       # 支持 doc[-1] 负索引
      d = page.get_text("dict")           # fitz 兼容 dict（文本块 type=0、图像块 type=1 内联）
      t = page.get_text("text")           # 纯文本
      bs = page.get_text("blocks")        # fitz "blocks" 元组列表
      imgs = page.get_images()            # 该页图像
      print(page.is_scanned, page.rect, page.number)
  ```

  - **open() 策略**：一次性并行提取所有页（eager），后续访问纯内存，零延迟
  - **向后兼容**：`extract()` / `extract_many()` 完全不变，仍用于批量场景
  - **fitz 对齐细节**：span 输出 `bbox/text/font/size/color/flags` 六个核心字段；
    `flags` 暂为 `0` stub（不带 italic/bold 格式探测），不影响字段访问；
    `ascender/descender/origin` 等 fitz 扩展字段不输出（已在 README 标注）
  - **type=1 image block**：fitz 把图像块和文本块混在同一 `blocks` 数组里，
    flashpdf v0.2.0 同样如此（之前 extract() 是分离的两个 list）

### Changed

- `PageResult` 新增 `rect: [f64; 4]` 字段（从 /MediaBox 解析），供 `Page.rect`
  属性暴露。所有 `extract_page_batch` 兜底返回也填充默认 letter 尺寸。

### Tests

- 新增 2 个单元测试覆盖 `page_rect`：MediaBox 优先 + blocks union 兜底
- 总测试数 39 个全部通过

## [0.1.4] - 2026-06-24

### Added

- **扫描页检测（`is_scanned`）**：新增启发式判断每页是否为扫描页——
  页内可提取文本字符 < 50 **且** 存在覆盖页面 ≥ 70% 的位图。flashpdf
  不做 OCR，但识别扫描页后可以把原始图像字节交给外部 OCR 引擎。

  - `PageResult.is_scanned: bool`（Rust）
  - `extract(..., with_page_info=True)` 返回 3-tuple `(blocks, images, pages)`，
    其中 `pages = [{"page": 0, "is_scanned": False}, ...]`
  - 默认 `with_page_info=False`，**完全向后兼容**现有 `(blocks, images)` 解包
  - 对混合文档（部分电子 + 部分扫描）按页分别判断
  - 新增 5 个单元测试覆盖：纯扫描页、纯文本页、低字符数+全图、小图（logo）、空页

## [0.1.3] - 2026-06-23

本次发布聚焦**解码准确性**与**行内空格还原**，char_sim 从 v0.1.2 的 21%
跃升到 **95%+**，达到与 PyMuPDF 对齐的水平。

### Fixed

- **ToUnicode 多码点映射**：bfchar/bfrange 的 unicode 值可能编码多个字符
  （UTF-16BE 码元序列，含代理对）。新增 `decode_chars` 将 ToUnicode 字节按
  UTF-16BE 解码为 `Vec<char>`，并通过按比例分配字形宽度正确还原字符序列。
  修复 TeX Computer Modern 字体（CMSY/CMMI/CMR）常见多字节 ToUnicode。

- **尊重字体的连字映射**：之前强制把 `ﬁ → fi`、`ﬂ → fl`、`ﬃ → ffi` 等
  Adobe 连字展开为 ASCII，会破坏 PyMuPDF 的"按字体声明输出"行为。移除强制
  扩展块，保留解码器返回的字面字符，与 PyMuPDF 一致。

- **嵌入式 Type1 字体 /Encoding 恢复**：TeX CM 字体没有 PDF /Encoding，也没
  有 /ToUnicode，但通常嵌入了 PFA/PFB 字体程序。新增
  `extract_encoding_from_font_program`：解析 FontDescriptor → FontFile(2/3)，
  去压缩流，剥离 PFB 段头 (0x80 前缀)，用 memchr 扫描 `/Encoding ... def`
  段还原 256 项编码向量。配合 adobe-glyph-list 翻译字形名为 Unicode。

- **拼接 <hex> token 的 CMap 解析**：现代 Office 字体（Aptos 等）的
  ToUnicode 把多个 bfchar 项拼接在一行 `<21><21><0041>`，无空格分隔。旧的
  按空白分割的解析器会把整行视作单个 token 导致码点查找不到（byte 0x31
  报 "NOT FOUND"）。改用扫描 `<...>` 分隔符的 `extract_hex_tokens`，正确
  解析所有拼接形式。

- **Tj/TJ 字距触发空格**：TJ 操作符的字距调整值（< -150/1000 em）插入
  合成空格，修复单词粘连。

- **行内 Td/Tm 画笔跳动空格**：当 Td/Tm 在同一行内（dy 小）水平移动
  0.15-1.0 em 时，发出一个空格。修复字体切换处（如 "Woo et al."）漏掉的
  词间空格。使用 tlm/tm 矩阵计算真实画笔位移，严格区间避免污染数学公式。

- **未映射控制字符直通**：raw byte < 0x20 在无映射时直出原始字节而非
  U+FFFD，减少替换符噪声。

- **TJ 字距阈值收紧**：标题/缩进产生的大字距被误判为词边界，阈值收紧
  到 0.15-0.6 em 区间。

- **阅读顺序（MuPDF 风格）**：v0.1.2 的 block 级 XY-cut 只把 char_sim 从 18%
  提到 21%，根因不在列检测算法，而在 `build_lines` 入口对 spans 做了 `(y, x)`
  预排序——这摧毁了 PDF 内容流本来正确的发射顺序。借鉴 MuPDF
  `stext-device.c` 的在线处理思路，做了两处修改：

  - `build_lines`：删除 `(y, x)` 预排序，让 spans 按 cluster_chars 输出顺序
    （即内容流顺序）进入后续的 same-line / column-gap 判断。
  - `build_blocks`：原 gap 检查 `curr_top - prev_bottom` 是有符号的，仅在
    下一条 line 视觉上高于上一条时才为正（依赖旧的 y 升序预排序）。改为
    方向无关的垂直空白公式 `max(y0_a, y0_b) - min(y1_a, y1_b)`，正确支持
    流序中"视觉从上到下"（y 递减）的 line 序列。

  前提是排版规范的 PDF（包括 arXiv 论文）在内容流里本来就按阅读顺序发射
  text 对象，这与 MuPDF 的设计假设一致。

### Benchmark 影响

| 文件 | char_sim (v0.1.2) | char_sim (v0.1.3) | trigram | FFFD |
|------|---------:|---------:|---------:|---------:|
| dbnet_plus | 21% | **96.8%** | 91.4% | 1 |
| arxiv_2604 | 21% | **95.5%** | 88.1% | 0 |

性能无回归：文本提取 **15-33x** PyMuPDF；文本 + 图像 **17-33x**。

### Tests

- 新增数学字形名（asteriskmath、circlemultiply、circleplus、circumflex、
  equivalence、existential、openbullet、prime、propersubset/superset、
  reflectequiv、similar、universal）扩展 adobe-glyph-list。
- 单元测试 32 个全部通过。


## [0.1.2] - 2026-06-19

### Added

- **阅读顺序优化（recursive XY-cut）**：在 `cluster_chars` 输出的 block 列表
  上做后处理排序，解决复杂版面（标题 + 摘要 + 双栏正文）下输出顺序与视觉
  阅读顺序不一致的问题。

  算法：递归 XY-cut（Nagy），先尝试水平切（分离标题带与正文带），再尝试
  垂直切（分离左右栏），切不动时按 (y_top DESC, x_left ASC) 兜底排序。
  PDF y-up 坐标系下，y 越大越靠上，先输出。

  另加防御性过滤：丢弃 bbox 远超页面（如向量图形被误聚类为文字）的 block，
  避免 XY-cut 的 gap 检测被污染。

  benchmark 影响：char_sim ~18% → ~21%；trigram Jaccard 与内容覆盖基本不变。
  改善有限的主要原因是上游 `detect_columns_from_spans` 在含大量公式的页面
  上检测失败，导致单个 block 横跨双栏 —— XY-cut 在 block 级无法修复该问题，
  需要后续在 span 级引入 XY-cut 或重写列检测。

### Tests

- 新增 3 个单元测试覆盖 XY-cut：标题 + 双栏顺序、单栏 yx 兜底、空/单元素

## [0.1.1] - 2026-06-19

### Fixed

- **内置字体编码支持**：为没有 ToUnicode CMap 的字体增加内置编码表，
  解决常见字符显示为 `U+FFFD` 替换符的问题：

  - **Adobe Symbol**（PDF 14 标准字体）：希腊字母、数学符号
  - **Adobe ZapfDingbats**（PDF 14 标准字体）：dingbats 装饰符号
  - **TeX CMSY**（LaTeX Computer Modern Symbol）：`•` (bullet)、
    `×` (multiply)、`′` (prime) — 学术论文中作者机构分隔符等常见字符

  `decode_char` 的 fallback chain 现在依次尝试：ToUnicode CMap →
  Encoding Differences → 内置字体编码 → raw byte → `U+FFFD`。

  以 `dbnet_plus.pdf` 为基准，`U+FFFD` 字符数从 152 降到 99
  （修复 53 个字符，包括作者机构行之间的 bullet）。性能无回归。

### Tests

- 新增 3 个单元测试覆盖 Symbol、ZapfDingbats、CMSY 解码路径

## [0.1.0] - 2026-06-19

### Added

- 首次 PyPI 发布
- Rust 核心 + Python 绑定（maturin/pyo3）
- 自研 PDF 解析器（~800 行）：对象解析、xref 表/流/ObjStm、memchr fallback
- 内容流状态机：BT/ET 文本块、Tj/TJ、Td/TD/Tm、Form XObject 递归、Do 图像
- 字体处理：CMap (bfchar/bfrange)、Type0 CIDFont、Encoding Differences、Adobe Glyph List
- 布局分析：chars → spans → lines → blocks
- 图像提取：JPEG/JPX 零拷贝、FlateDecode 惰性 PNG、四角变换 bbox
- 并行调度：rayon 页级并行、文件级并行、异步预读、大文档自动分批
- 多平台预编译 wheel（Linux x86_64/aarch64、macOS x86_64/ARM64、Windows x86_64）
- 通过 PyPI Trusted Publishers 实现 tag 触发的自动化发布
