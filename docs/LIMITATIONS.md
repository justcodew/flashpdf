# flashpdf 短板清单

flashpdf 在文本提取和页面渲染两个场景是最快的（见 [README 基准](../README.md#基准)
和 [BENCHMARK_RENDER.md](BENCHMARK_RENDER.md)），但**不是所有 PDF 任务都最快，
也不是所有 PDF 任务都做**。本文系统化列出已知短板，避免用户误用。

## TL;DR：选择 flashpdf 之前先确认这些

| 你的需求 | flashpdf 行不行 |
|---|---|
| 文本提取（blocks/lines/spans + bbox/字体/字号/颜色）| ✅ 最快 |
| 页面渲染 + PNG 编码 | ✅ 最快（需 `render` feature + PDFium binary）|
| 嵌入图像提取（JPEG/PNG/JPX）| ✅ 快，但**没对比 benchmark** |
| 加密 PDF（AES-256 / 需要密码）| ❌ 只支持 RC4 / AES-128 空密码 |
| PDF 编辑（合并/拆分/加页/删页/表单填写/签名）| ❌ 完全不做 |
| OCR | ❌ 不做（只识别扫描页，不解码文字）|
| 注释、表单字段、富媒体 | ❌ 不做 |
| 字体度量精度（ascender/descender/origin、sub/superscript）| ❌ 不输出 |
| italic/bold 字体探测 | ⚠️ 部分支持（名字启发式，不读 /FontDescriptor /Flags）|
| 矢量图光栅化、页面布局重排 | ❌ 不做 |
| < 1KB 极小文件批量 | ⚠️ 优势消失（启动开销主导，与 pdf_oxide 持平）|

## 1. 加密 PDF 限制（重要）

flashpdf 只支持 **RC4（V1/V2, R=2/3）** 和 **AES-128（V4, R=4）**，**且只接受空用户密码**。

具体限制：

- ❌ **AES-256（V5/R6, PDF 2.0）不支持**——直接抛 `ValueError`
- ❌ **非空密码不支持**——没有 `password=` 参数，加密码的 PDF 全部读不了
- ❌ **所有权密码（owner password）不支持**
- ❌ **加密元数据流 (`/EncryptMetadata`) 不单独处理**

代码位置：`crates/flashpdf-core/src/crypto.rs:3-7`

```rust
// AES-256 (V5/R6) is detected but reported as unsupported.
```

**对比**：PyMuPDF / pypdfium2 / pypdf 全套支持，包括 AES-256 和任意密码。

**影响范围**：现代 Word/Office 导出的加密 PDF、企业级文档管理系统产物、
部分政府/法律 PDF 用 AES-256——这些 flashpdf 都打不开。

## 2. `span.flags` 部分支持（精度短板）

README 里写"flags 暂为 stub"**已过时**——v0.7 实际有部分支持，但精度不如 fitz。

实际行为（`crates/flashpdf-core/src/font.rs:556` `compute_font_flags`）：

| 字段 | 检测方式 | 准确度 |
|---|---|---|
| italic (bit 2) | 字体名匹配 `*Italic*` / `*Oblique*`；**不读 /FontDescriptor /Flags**（call site 传 None） | ⚠️ 漏判标 Italic 位但名字不带 Italic 的字体 |
| serif (bit 4) | 字体名匹配 `*Times*` / `*Serif*` / `*Garamond*` / `*Palatino*` | ⚠️ 同上 |
| monospaced (bit 8) | 字体名匹配 `*Courier*` / `*Mono*` / `*Consolas*` / `*Menlo*` | ⚠️ 同上 |
| bold (bit 16) | 字体名匹配 `*Bold*` / `*Black*` / `*Heavy*` / `*Demi*` / `*Semibold*`（PDF /Flags 无 bold 位） | ⚠️ 名字不带 Bold 但实际粗体的字体漏判 |

**对比**：fitz 直接读 `/FontDescriptor /Flags` 位字段，更准。

**修复路径**：call site 应该传 `/FontDescriptor /Flags` 给 `compute_font_flags`，
但当前架构里 `extract_doc` 没把 /Flags 透传到字体映射。这是已知 enhancement，
不在 v0.7 路线图里。

## 3. fitz 扩展字段不输出

`span` 字段对齐 fitz **常用子集**，但不输出 fitz 的扩展字段：

| fitz 有 | flashpdf | 影响 |
|---|---|---|
| `bbox` / `text` / `font` / `size` / `color` / `flags` | ✅ 都有 | — |
| `ascender` / `descender` | ❌ 不输出 | 字体度量级排版分析做不了 |
| `origin` (字符起始点) | ❌ 不输出 | 字符级位置追踪做不了 |
| `block_no` / `line_no` / `span_no`（部分 mode）| ❌ 不输出 | 需要用户自己 enumerate |

如果你需要这些字段，用 fitz 而不是 flashpdf。

## 4. 完全不做的功能（设计目标）

flashpdf 是**纯只读数据提取库**——下列功能**永远不会做**：

### 编辑类
- ❌ 合并/拆分 PDF
- ❌ 添加、删除、重排页面
- ❌ 表单字段填写（AcroForm / XFA）
- ❌ PDF 数字签名
- ❌ 添加/编辑注释（高亮、便签、链接）
- ❌ 加密/解密 PDF（flashpdf 解密只用于内部读取，不写回）
- ❌ 元数据写入

### 渲染扩展
- ❌ 矢量图光栅化（页面里的曲线/路径不渲染）
- ❌ 表单字段渲染（输入框、按钮的视觉表示）
- ❌ 注释视觉渲染（高亮、下划线等）
- ❌ OCR（识别扫描页文字）
- ❌ 多种输出格式（JPEG / WebP / TIFF）—— `get_pixmap` 只返回 PNG

### 文档级
- ❌ 增量更新解析（/Prev xref chain 不完全跟随）
- ❌ 嵌入式文件流（/EmbeddedFile）提取
- ❌ 可访问性标记树（/StructTree）解析

## 5. 图像提取的局限

flashpdf 的图像提取**只针对嵌入位图**——`Do` 引用的 Image XObject。

✅ 支持：
- JPEG（DCTDecode）
- PNG（FlateDecode with PNG predictor）
- JPX（JPXDecode / JPEG2000）
- 原始字节直出（不解码、不重新压缩）

❌ 不支持：
- 矢量图（页面里的路径、曲线、填充）—— 不算"图像"
- 页面截图（"渲染这页为图片"用 `get_pixmap`，不是 `get_images`）
- 内联图像（/EI /W /IB）—— 当前实现跳过
- CCITT Fax 编码（CCFDecode）—— 老式扫描 PDF 可能有
- JBIG2 / RunLength 编码 —— 罕见但合法
- 嵌入式 ICC 配置文件提取 —— 颜色管理交给调用方
- /SMask 软掩膜分离 —— 直接内联到主图像，不单独提取

**未对比 benchmark**：vs PyMuPDF / pdfimages / pypdf 的图像提取速度**没测过**。
理论上跟随文本提取的速度（同一遍解析），但**没数据支撑"图像提取也最快"**。

## 6. 渲染功能的局限

`get_pixmap()` 是 PDFium 的薄封装，因此继承了 PDFium 的特性集 + 我们封装的限制：

### API 层
- ❌ **不返回 raw 像素**（只返回 PNG bytes）。要拿 RGBA 原始字节做后续处理
  的场景（OpenCV、numpy），需要先 decode PNG——不如 fitz `pix.samples` 直接
- ❌ 无 `clip` / `matrix` / `colorspace` / `alpha` 参数
- ❌ 无 PIL / numpy 互操作（用户自己 `PIL.Image.open(io.BytesIO(png))`）
- ❌ 输出恒为 RGBA + 白底（不保留透明度）

### 部署层
- ❌ **PDFium binary 不打包**：默认 `pip install flashpdf` 没有渲染能力。需要：
  1. 从源码 `maturin develop --release --features render`
  2. 单独下 PDFium binary（`PDFIUM_PATH` 或 `./pdfium-bin/`）
  
  对比 fitz：装完就能渲染——开箱即用体验上 fitz 占优
- ❌ 没有 `pip install "flashpdf[render]"` extras（roadmap）
- ❌ 没有 multi-platform wheel CI（roadmap）

### 性能层
- ⚠️ **极端吞吐场景未测**：每秒 1000+ 页缩略图这类场景，pypdfium2 的
  C 直调可能比 flashpdf 的 Rust → FFI → Rust 多一层更快。没数据
- ⚠️ **只测第 0 页**：渲染 benchmark 只渲染每文件第 0 页，长文档渲染
  所有页的吞吐量没测过

## 7. 极小文件场景优势消失

README 基准里 tiny 桶（< 10KB）数据：

| 库 | tiny p50 ms |
|---|---:|
| flashpdf | **0.092** |
| pdf_oxide | 0.093 |

**几乎持平**——不是显著领先。原因是极小文件场景**启动开销主导**（Python 解释器、
库 import、PDF header 解析），真正的提取逻辑占比很小。

`< 1KB` 的文件（纯邮件附件、极简表单）三家可能都是 0.05ms 量级，差距不显著。
这种场景选 flashpdf 没意义，选 pdf_oxide / fitz 都一样。

## 8. 字体子集化的精度问题

PDF 字体子集化（`ABCDEF+TimesNewRoman`）的常见问题：

- ✅ /ToUnicode CMap 解码（Unicode 反向映射）—— 支持
- ✅ /Encoding + Differences —— 支持
- ✅ Adobe Glyph List（AGL）—— 支持
- ⚠️ **无 /ToUnicode 的子集字体** —— 字节级解码可能丢字符（变 U+FFFD）。
  `page.diagnostics.undecoded_byte_count` 计数暴露这类情况
- ⚠️ **CID 字体宽度估算**：缺失 CID width 时退化为 default width，
  可能导致 bbox 不准
- ❌ **Type 3 字体定位不准**：Type 3 字形由绘制操作定义（不是轮廓），
  flashpdf 用 /Widths 解码但**字形几何可能错位**。

## 9. 测试覆盖的盲区

flashpdf 测试集中度高，但有些场景**没自动化测试覆盖**：

- ❌ **AES-256 加密 PDF**：库代码有 detection（报 unsupported），但没 e2e 测试
- ❌ **PDF 2.0 特性**（关联数组、Unicode 密码）：没测过
- ❌ **超大 PDF**（> 100MB）：bench corpus 最大 8.3MB，更大文件行为未知
- ❌ **多线程并发 `open()`**：rayon 内部并行测过，但用户级 threading 测试缺
- ❌ **图像提取正确性**：benchmark 测了**速度**，没测**像素级正确性**
  （解码出来的 JPEG 字节是否与 `pdfimages` 输出 bit-identical）

## 10. 已知 bug / 待修

短期内不会修但已知的：

- **`/Count` 在嵌套页树里不准**：v0.7.1 已修（三层 fallback），但极端边缘
  case（/Pages /Kids 引用循环）会无限递归栈溢出
- **xref 全表扫描恢复 O(N)**：10000+ 对象的 PDF 全表扫描可能 100ms+，
  但仅在主路径失败时触发

## 选型建议

| 你的场景 | 推荐 |
|---|---|
| 纯文本提取，大批量（RAG / 全文索引）| ✅ flashpdf |
| 同时要文本 + 渲染（混合工作流）| ✅ flashpdf |
| 仅渲染缩略图 | ⚠️ flashpdf（领先）或 pypdfium2（部署更简单）|
| 仅渲染 + 像素级后处理（OpenCV / OCR 输入）| ⚠️ fitz（pix.samples 直出）或 flashpdf + PNG decode |
| 需要编辑 PDF | ❌ 用 PyMuPDF / pypdf / pdfplumber |
| 需要 AES-256 / 加密码 PDF | ❌ 用 PyMuPDF / pypdfium2 |
| 极小文件批量（< 1KB）| 三家差不多，挑顺手 |
| 字体度量级精度（italic/bold/ascender）| ❌ 用 PyMuPDF |
| 学术级 Type 3 字形正确性 | ❌ 用 MuPDF / pdfium 直接（不是封装层）|
