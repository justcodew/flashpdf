# Changelog

## [0.1.3] - 2026-06-22

### Fixed

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

  benchmark 影响：
  - char_sim：21% → **66-70%**（dbnet 66.2%，arxiv_2604 70.2%）
  - trigram Jaccard：53% → **65-68%**
  - 性能无回归（仍 22-34x）

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
