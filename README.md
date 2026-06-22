# flashpdf

世界上最快的 PDF 文本与图像提取引擎。

Rust 核心 + Python 绑定，输出与 PyMuPDF 兼容的 `blocks` 和 `images` 结构。

## 特性

- **极致性能**：全链路零拷贝 (mmap)、SIMD 字节扫描 (`memchr`)、快速浮点解析 (`fast-float`)
- **不牺牲信息**：完整的文本提取链路，包括 CMap、Type0 复合字体、Form XObject 递归
- **并行处理**：rayon 页级并行 + 文件级并行 + 异步预读
- **健壮容错**：xref 损坏时自动 memchr 全文扫描恢复
- **PyMuPDF 兼容**：输出结构与 PyMuPDF 完全一致，零迁移成本

## 安装

```bash
pip install flashpdf
```

从源码构建：

```bash
# 需要 Rust 工具链 (https://rustup.rs)
git clone https://github.com/yourname/flashpdf.git
cd flashpdf
pip install maturin
maturin develop --release
```

## 快速开始

### Python

```python
import flashpdf

# 单文档提取
blocks, images = flashpdf.extract("document.pdf")

for block in blocks:
    for line in block["lines"]:
        for span in line["spans"]:
            print(f"[{span['font']} {span['size']:.0f}] {span['text']}")

for img in images:
    print(f"Image: {img['width']}x{img['height']} {img['ext']}")
    # img['image'] 是原始字节 (JPEG/PNG)

# 批量提取 (文件级并行)
for path, blocks, images in flashpdf.extract_many(
    ["a.pdf", "b.pdf", "c.pdf"],
    file_parallel=True,
    include_images=False
):
    print(f"{path}: {len(blocks)} blocks")
```

### Rust

```rust
use flashpdf_core::{extract, ExtractOptions};

let options = ExtractOptions {
    page_parallel: true,
    include_images: true,
    batch_size: 50,
    ..Default::default()
};

let result = extract("document.pdf", &options)?;

for page in &result.pages {
    for block in &page.blocks {
        for line in &block.lines {
            for span in &line.spans {
                println!("[{} {:.0}] {}", span.font, span.size, span.text);
            }
        }
    }
}
```

## API 参考

### `flashpdf.extract(path, **options)`

从单个 PDF 文件提取文本和图像。

**参数：**

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `path` | `str` | *必填* | PDF 文件路径 |
| `page_parallel` | `bool` | `True` | 页级并行（多核加速） |
| `include_images` | `bool` | `True` | 是否提取图像数据 |
| `gpu` | `bool` | `False` | GPU 加速（需要 NVIDIA GPU） |
| `batch_size` | `int` | `50` | 大文档分批大小（0=不分批） |

**返回值：** `(blocks, images)`

#### blocks 结构

```python
[
    {
        "type": 0,                    # 0 = 文本块
        "bbox": (x0, y0, x1, y1),    # 块边界框
        "lines": [
            {
                "bbox": (x0, y0, x1, y1),
                "spans": [
                    {
                        "bbox": (x0, y0, x1, y1),
                        "text": "Hello World",
                        "font": "Helvetica",
                        "size": 12.0,
                        "color": 0,
                    }
                ]
            }
        ]
    }
]
```

#### images 结构

```python
[
    {
        "bbox": (x0, y0, x1, y1),    # 页面中的位置
        "width": 1920,                # 像素宽度
        "height": 1080,               # 像素高度
        "bpc": 8,                     # 每通道位数
        "colorspace": "DeviceRGB",    # 色彩空间
        "xref": 42,                   # 对象编号
        "ext": "jpeg",                # 格式: jpeg/png/jpx
        "image": b"\xff\xd8\xff...",   # 原始字节 (None 如果 include_images=False)
    }
]
```

### `flashpdf.extract_many(paths, **options)`

批量提取多个 PDF 文件。

**参数：**

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `paths` | `list[str]` | *必填* | PDF 文件路径列表 |
| `file_parallel` | `bool` | `True` | 文件级并行 |
| `page_parallel` | `bool` | `False` | 页级并行（与 file_parallel 互斥时建议关闭） |
| `include_images` | `bool` | `False` | 是否提取图像 |
| `gpu` | `bool` | `False` | GPU 加速 |
| `batch_size` | `int` | `50` | 大文档分批大小 |

**返回值：** `[(path, blocks, images), ...]`

## 架构

详见 [API 文档](docs/API.md) 获取完整的 API 参考。设计文档见 [DESIGN_V1](docs/DESIGN_V1.md) 和 [DESIGN_V2](docs/DESIGN_V2.md)。



```
PDF 文件
  │
  ├─ mmap 映射 (零拷贝)
  │
  ├─ 自研解析器 (~800 行)
  │   ├─ 对象解析 (递归下降)
  │   ├─ xref 表/流/ObjStm
  │   └─ memchr fallback (xref 损坏恢复)
  │
  ├─ 内容流状态机
  │   ├─ BT/ET 文本块
  │   ├─ Tj/TJ 文本操作符
  │   ├─ Td/TD/Tm 矩阵变换
  │   ├─ Form XObject 递归 (深度 3)
  │   └─ Do 图像捕获
  │
  ├─ 字体处理
  │   ├─ CMap 解析 (bfchar/bfrange)
  │   ├─ Type0 复合字体 (CIDFont)
  │   ├─ Encoding Differences
  │   └─ Adobe Glyph List
  │
  ├─ 布局分析
  │   └─ chars → spans → lines → blocks
  │
  ├─ 图像提取
  │   ├─ JPEG/JPX 零拷贝 (mmap 切片)
  │   ├─ FlateDecode 惰性 PNG
  │   └─ 四角变换 bbox
  │
  └─ 并行调度
      ├─ rayon 页级并行
      ├─ 文件级并行
      ├─ 异步预读
      └─ 大文档自动分批
```

## 性能目标

| 场景 | 目标 | 实际 (v0.1.1) |
|------|------|------|
| 文本提取 | ≥ PyMuPDF 2x | **~20-50x** (视 PDF 复杂度) |
| 文本 + 图像提取 | ≥ PyMuPDF 5x | **~20-34x** ✅ |
| 字符总量 | 与 PyMuPDF 接近 | 差异 <2% |
| 吞吐量 | — | 2000-2800 pages/sec |
| 单词 Jaccard 重叠率 | ≥ 50% | **45-49%**（详见下方"已知限制"） |

> **已知限制**：flashpdf 在 v0.1.2 引入了 recursive XY-cut 阅读 order 排序
> （block 级后处理），char-level 相似度从 ~18% 提升到 ~21%。剩余差距主要来自
> 上游 `detect_columns_from_spans` 在公式密集的页面（如 arXiv 论文）上检测
> 失败，导致单个 block 横跨双栏，block 级 XY-cut 无法修复。后续计划在 span
> 级引入 XY-cut 或重写列检测以彻底解决。

完整对比（性能 + 精度 + 结构）见 [性能基准报告](docs/BENCHMARK.md)。

## 测试

```bash
# 运行全部测试
cargo test -p flashpdf-core

# 运行特定测试
cargo test -p flashpdf-core test_cmap

# 性能基准
cargo bench -p flashpdf-core
```

当前测试：**85 个测试全部通过** ✅

- 对象解析器：45 个测试
- xref + trailer：11 个测试
- 内容流 + 布局 + 字体 + recovery：26 个测试
- 流解码器 (LZW/ASCII85/RunLength/ASCIIHex)：3 个测试

## 依赖

| Crate | 用途 |
|-------|------|
| `memchr` | SIMD 字节扫描 |
| `fast-float2` | 快速浮点解析 |
| `flate2` | zlib 解压 |
| `memmap2` | 零拷贝文件映射 |
| `rayon` | 并行迭代器 |
| `pyo3` | Python 绑定 |
| `crc32fast` | PNG CRC 校验 |
| `fnv` | 快速哈希 |
| `smallvec` | 小数组优化 |

## 路线图

- [x] 阶段 1: 自研 PDF 解析器
- [x] 阶段 2: 内容流解析 + 字体处理
- [x] 阶段 3: 布局分析
- [x] 阶段 4: 图像提取
- [x] 阶段 5: 并行化 + I/O 优化
- [x] 阶段 6: 性能基准 + PyMuPDF 对比测试
- [x] 阶段 7: PyPI 发布 + CI/CD

详见 [TODO.md](TODO.md) 获取完整的待完成事项列表。

## 许可证

MIT
