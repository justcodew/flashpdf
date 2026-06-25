# Changelog

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
- **旋转文本提取（`include_rotated`）**：新增 `ExtractOptions::include_rotated`
  字段，`open()` / `extract()` / `extract_many()` 均新增同名 Python 关键字参数，
  默认 `False`。开启后能正确提取 arXiv 侧栏水印（`arXiv:xxxx [category] date`）
  和图表纵轴标签等通过 `cm`/`Tm` 旋转的文本。

  - 旋转字符通过 TRM = CTM × Tm 的 4-角变换计算 bbox，方向向量驱动 advance
  - 标准 14 字体无 `/Widths` 时，per-char advance 取 0.5em 经验值，
    避免 40 字符侧栏跨出页面边界被阅读序过滤器丢弃
  - 旋转字符独立聚类并**追加到页 block 列表末尾**，不进入 XY-cut 排序，
    正文 char_sim 与 default 行为字节级保持一致

### Changed

- `PageResult` 新增 `rect: [f64; 4]` 字段（从 /MediaBox 解析），供 `Page.rect`
  属性暴露。所有 `extract_page_batch` 兜底返回也填充默认 letter 尺寸。
- `CharInfo` 新增 `rotated: bool` 字段，标记该字符是否在非轴对齐文本矩阵下
  生成（`Tm` 或 `ctm` 的 b/c 分量非零）。

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
