# PDF 文本与图像提取引擎（flashpdf）— 终极性能版开发任务书

> **使命**：打造世界上速度最快、且**不牺牲任何文本/图像提取信息**的 PDF 提取引擎。  
> **目标**：Rust 实现极致性能，Python 绑定，输出与 PyMuPDF (fitz) 结构兼容的 `blocks` 和 `images`，支持多页/多文件并行、多层次优化（含可选 GPU 加速）。  
> **资源**：充裕的人力与时间，追求理论极限。

---

## 1. 技术栈（含性能优化选型）

| 类别 | 技术 | 用途 | 性能收益 |
|------|------|------|----------|
| PDF 解析 | `lopdf` (初期) / 自研解析器 (长期) | 对象/流/交叉引用表 | 长期可消除无关抽象 |
| 内容流扫描 | `memchr` (SIMD) | 快速定位操作符标记 | 字节扫描 2-5x 提速 |
| 数字解析 | `fast-float` | 浮点数转换 | 比标准库快 2-3x |
| 哈希表 | `FnvHashMap` + 宽度表 `Vec<f32>` | 字体缓存、CID→宽度 | 哈希查找常数级优化 |
| 小数组 | `SmallVec` | 聚类临时存储 | 减少堆分配 |
| PNG 编码 | `zune-png` (可选) | 更快的 DEFLATE | 比 `image` crate 编码提速 |
| 并行 | `rayon` | 页级/文件级并行 | 多核线性加速 |
| Python 绑定 | `pyo3` | 导出函数，释放 GIL | 多线程无锁竞争 |
| I/O | `memmap2` | 零拷贝文件映射 | 减少系统调用和内存拷贝 |
| GPU 加速 (可选) | `nvjpg` / `nvcomp` (通过 FFI) | 硬件 JPEG 解码、DEFLATE 解压/压缩 | 图像处理可达数量级提升 |

## 2. 架构与关键设计决策

### 2.1 架构流程
```
PDF 文件 → mmap 映射
       → 页面快照提取 (零拷贝 Arc<[u8]>，解耦线程安全)
       → 全局字体缓存 (FnvHashMap + 预计算 O(1) 数组)
       → 内容流解析 (SIMD 扫描 + fast-float + 宽容状态机)
            ├─ Form XObject 递归 (最大深度 3)
            └─ Do 图像捕获 (只存 mmap 偏移+长度)
       → 在线布局聚类 (可选，边解析边产出 Span/Line，内存 -30%)
       → 图像提取 (JPEG/JPX 返回 mmap 切片，Flate 惰性编码；可选 GPU 加速)
       → 并行调度 (rayon 页级/文件级，异步预读，内存保护分批)
```

### 2.2 五大强制决策

| 决策点 | 默认方案 | 变更/触发条件 |
|--------|----------|---------------|
| **lopdf 线程安全** | 页面快照模式（不共享 `Document`） | 若验证 `Document: Send+Sync`，可用 `Arc` 共享 |
| **字体编码边界** | 仅标准字体 (阶段 2a) | 阶段 2a 测试中文本丢失率 > 5% 时，**必须启动 2b** |
| **Form XObject** | 至少展开一层递归，否则警告 | 不可静默跳过 Form 中的文本/图像 |
| **内存控制** | 大文档+页并行时自动分批（页数>100 时每批 50 页），`include_images=False` 推荐 | 用户可通过 `FLASHPDF_BATCH_SIZE` 调整或设为 0 强制不分批 |
| **鲁棒性** | 宽容解析：未知操作符/畸变流只警告，绝不 panic；字符解码失败降级到 U+FFFD | — |

## 3. 核心算法摘要

### 3.1 内容流解析
- 操作符状态机维护：CTM、Tm、Tlm、当前字体/字号、填充颜色等。
- 字符位移：`tx = (font_width * font_size * horizontal_scale) / 1000`，通过 Tm 定位，CTM 变换，翻转 y 轴。
- Form XObject 递归：文本与图像共用入口，最大深度 3。
- **微观优化**：使用 `memchr` 定位 `BT`/`ET`/`Tj`/`TJ`/`Do` 等标记；`fast-float` 解析数字。

### 3.2 字体缓存与降级
- 全局 `FnvHashMap<ObjectId, FontInfo>`，其中字体宽度用 `Vec<f32>` 按 CID 直接索引。
- 降级路径：**ToUnicode CMap → Encoding 差异表 → 原始字节映射 → U+FFFD**。
- **字体扩展 (2b)**：预计算 Unicode 映射数组，O(1) 字符解码。
- **子集化**：只加载页面实际使用的 CID 宽度，减小缓存。

### 3.3 布局聚类（在线/离线可选）
- **传统两遍法**（默认）：先收集所有字符，再单遍聚类 chars→spans→lines→blocks。
- **在线聚类**（进阶 feature）：状态机边解析边产出 Line，无需全量字符存储，内存峰值 -30%，延迟更低。
- 聚类规则：Span = 同字体/字号/颜色 + 几何邻近；Line = 垂直接近的 Span；Block = 大垂直间隙。

### 3.4 图像提取（零拷贝 + 惰性 + GPU）
- 捕获 `Do`，记录图像字典和 **mmap 偏移/长度**（不拷贝字节）。
- 四角变换 bbox，支持旋转/剪切，翻转 y。
- **惰性输出**：JPEG/JPX 直接返回 mmap 切片；Flate/LZW 编码 PNG 延迟到访问时。
- **GPU 加速**（可选 feature）：当 `gpu=True` 时，调用 `nvjpg` 硬件解码 JPEG、`nvcomp` 加速 DEFLATE。若无 GPU，自动回退 CPU。

## 4. API 设计（与 PyMuPDF 兼容）

```python
import flashpdf

# 单文档提取
blocks, images = flashpdf.extract(
    "doc.pdf",
    page_parallel=True,      # 页级并行
    include_images=True,     # 是否填充图像字节
    gpu=False                # 是否尝试 GPU 加速图像处理
)

# 批量提取（流式迭代器，低内存）
for path, blocks, images in flashpdf.extract_many(
    ["a.pdf", "b.pdf"],
    file_parallel=True,      # 文件级并行
    page_parallel=False,     # 内部页面串行 (避免嵌套并行)
    include_images=False,    # 推荐关闭以控制内存
    gpu=False
):
    process(path, blocks, images)
```

### 输出数据结构（完全匹配 PyMuPDF 格式）

```python
# 文本
Block {
    type: 0,
    bbox: (x0, y0, x1, y1),
    lines: [Line {
        bbox: (x0, y0, x1, y1),
        wmode: 0,
        dir: (1.0, 0.0),
        spans: [Span {
            bbox: (x0, y0, x1, y1),
            text: "Hello",
            color: 0,
            font: "Helvetica",
            size: 12.0,
            chars: [Char { bbox: (x0, y0, x1, y1), c: "H" }, ...]
        }]
    }]
}

# 图像
[{
    "bbox": (x0, y0, x1, y1),
    "width": 1920,
    "height": 1080,
    "bpc": 8,
    "colorspace": "DeviceRGB",
    "xref": 42,
    "ext": "jpeg",           # "jpeg" / "png" / "jpx"
    "image": bytes           # 或 None，或 mmap 切片（零拷贝）
}, ...]
```

## 5. 性能优化路线图（全部内置，不牺牲信息）

### 第一层：微观优化（融入阶段 2a/3，默认开启）
- SIMD 字节扫描 (`memchr`)：操作符定位 2-5x 加速
- 快速浮点解析 (`fast-float`)：数字转换 2-3x 加速
- 哈希/数组优化：`FnvHashMap` + `Vec<f32>` 宽度直索引
- `SmallVec`：聚类存储减少 malloc

### 第二层：架构优化（融入阶段 4/5）
- **零拷贝图像**：mmap 切片，不复制像素数据
- **字体子集化**：仅加载页面使用的 CID 宽度
- **异步预读**：后台线程提前 mmap 下一个文件，隐藏 I/O
- **在线聚类**（可选 feature）：内存 -30%，延迟降低

### 第三层：极限优化（长期，可选 feature）
- **自研 PDF 解析器**：去除 lopdf 非必要抽象，定制内存布局
- **GPU 加速**：`nvjpg` 硬件 JPEG 解码，`nvcomp` 加速 DEFLATE；可配置开关，默认关闭

## 6. 开发阶段与任务（总计约 16 天）

### 阶段 1：基础框架 (2天)
- [ ] 项目搭建，引入所有依赖（含性能优化 crate）
- [ ] 验证 `lopdf::Document: Send + Sync`；若不满足，立即实现页面快照模式（零拷贝 `Arc<[u8]>`）
- [ ] 快照遍历耗时 < 总解析时间的 5%
- [ ] >1GB 文件 memmap2 加载测试
- [ ] pyo3 基础模块：返回页数

### 阶段 2a：内容流解析器—基础版 (2天)
- [ ] 实现宽容状态机，内嵌 **memchr** 和 **fast-float**
- [ ] 标准字体支持，**FnvHashMap** 缓存，宽度表 `Vec<f32>` 按 CID 索引
- [ ] 字符定位 & bbox（CTM + y 翻转）
- [ ] **必须预留 Form XObject 递归入口**，至少展开一层，未展开则警告
- **交付标准**：80% 现代 PDF 文本正确提取

### 阶段 2b：字体扩展 (2天，条件触发)
- **触发条件**：阶段 2a 完成后，在 20+ 样本上计算：
  ```
  丢失率 = 1 - (成功解码 Unicode 字符数 / Tj/TJ 总调用字符数)
  ```
  若 > 5%，**必须启动 2b**；否则可选。
- [ ] CMap 最小解析 (bfchar, bfrange)
- [ ] Type0 复合字体降级
- [ ] 预计算 Unicode 映射数组，O(1) 查找
- [ ] 完善降级路径：ToUnicode → Encoding → 原始字节 → U+FFFD

### 阶段 3：布局分析 (2天)
- [ ] 基础 chars→spans→lines→blocks 聚类（使用 `SmallVec`）
- [ ] 暴露可调参数：垂直间距阈值、字号缩放因子
- [ ] **在线聚类（可选 feature）**：修改状态机直接产出 Line，内存 -30%。通过 feature flag 切换，先确保传统两遍法正确。

### 阶段 4：图像提取与 GPU 加速 (2天)
- [ ] 状态机中捕获 `Do`，获取图像字典
- [ ] **零拷贝图像**：记录 mmap 偏移和长度，JPEG/JPX 直接返回 mmap 切片
- [ ] 四角变换 bbox 计算（含旋转/剪切），坐标翻转验证 min_y ≤ max_y
- [ ] 惰性 PNG 编码（使用 `zune-png` 提速）
- [ ] Form 递归深度限制 3
- [ ] **GPU 加速集成**（可选 feature）：
  - 引入 `gpu` feature，依赖 NVIDIA 库
  - 当 `gpu=True` 时，检测 GPU，使用 `nvjpg` 解码 JPEG、`nvcomp` 处理 DEFLATE
  - 无 GPU 自动回退 CPU，用户无感知

### 阶段 5：并行化与 I/O 优化 (2天)
- [ ] 页面级并行：rayon par_iter 处理快照，共享字体缓存 (Arc)
- [ ] 文件级并行：extract_many 实现，默认文件并行+页面串行
- [ ] GIL 释放：所有 Rust 函数通过 `allow_threads` 调用
- [ ] **异步预读**：后台线程提前 mmap 下一个文件
- [ ] **大文档内存保护**：当 `page_parallel=True`、`include_images=True` 且页数 > 100 时，自动分批并行（每批 50 页）。用户可通过 `FLASHPDF_BATCH_SIZE` 环境变量调整，设为 0 强制不分批。

### 阶段 6：性能极致化与综合测试 (3天)
- [ ] 用 flamegraph 定位并消除最后的热点
- [ ] 验证零拷贝图像有效性（mmap 切片生命周期）
- [ ] 在线聚类稳定性回归测试（若启用）
- [ ] GPU 加速路径正确性对比（与 CPU 输出完全一致）
- [ ] **对比测试 (20-30 份真实 PDF)**：
  - 纯文本（论文）5-8
  - 图文混排（杂志）5-8
  - 扫描件（仅图像）3-5
  - 表格密集型 3-5
  - 东亚文档（竖排/特殊字体）2-3
- [ ] **结构 Diff 脚本**：自动递归比较 flashpdf 与 PyMuPDF 输出，统计：
  - 块/行/span 数量差异率
  - bbox 平均绝对误差 (MAE)
  - 文本内容一致性（忽略空格差异）
  - 图像数量 & ext 匹配率
- [ ] **性能基准报告**：
  - 与 PyMuPDF / pypdfium2 对比，列出各场景加速比
  - 页级并行加速比（目标 2-4x）
  - GPU 加速收益（预期图像密集型场景数倍提升）
- [ ] 内存泄漏检查 (valgrind/heaptrack)

### 阶段 7：文档与发布 (1天)
- [ ] README（安装说明、快速开始、性能对比图表）
- [ ] API 完整文档
- [ ] 发布 wheel：
  - 默认版本 (无 GPU)：Linux x86_64, macOS ARM64
  - GPU 加速版本：单独说明依赖（CUDA 库）
  - 通过 GitHub Actions 自动构建
  - 按需上传 PyPI

## 7. 并行协作与关键路径

- **2a 与 4 可并行**：2a 状态机已能捕获 `Do` 并提取图像字典，可直接交接给阶段 4，无需等待阶段 3。
- **阶段 3 与 4 完全独立**，可由不同开发者同时推进。
- **文本丢失率测量**：2a 完成后立即执行，决定是否触发 2b，避免阻塞后续。
- **GPU 加速**：前期只需预留接口和 feature flag，主逻辑稳定后再集成实际 CUDA 调用。

---

**任务书思想与要求：追求极致速度、不牺牲信息、多层次优化、GPU 可选加速、充足人力下的完整蓝图。**
