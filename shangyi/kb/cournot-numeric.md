---
id: cournot-numeric
name: Cournot 数量竞争（数值路径）
kind: theory
family: noncooperative
summary: 当产能是主要约束时，竞争发生在产量而非价格上，均衡用最佳反应迭代求解。
applicable_when:
  - 存在显著的产能约束（扩产需要时间与资本）
  - 产量或产能是可观察的决策变量
  - 需求价格弹性可估计
fails_when:
  - '产能可以快速且低成本地调整（此时价格才是决策变量，转 [[differentiated-bertrand]]）'
  - '产品高度差异化且各自有独立需求（转 [[differentiated-bertrand]]）'
data_needs:
  - 各方的产能上限与边际成本
  - 市场需求函数（截距与斜率，或点估计）
outputs:
  - 均衡产量组合
  - 均衡价格与利润
---

# 为什么需要"数值路径"

线性需求 + 常数边际成本的情形有解析解（$q_i^* = (a - c_i)/(n+1)$ 形式）。
但真实场景里成本常是分段线性、产能带上限，此时最佳反应函数是**分段**的，
解析解不存在。

在这类情形下，商奕走 [[monte-carlo]] 传播参数不确定性 +
[[sensitivity-scan]] 定位翻转阈值，而不是硬套解析公式。

# 与 Bertrand 的切换判据

**"如果降价，能不能立刻多卖？"**
- 能（产能跟得上）→ 价格是决策变量 → Bertrand 族
- 不能（产能卡住）→ 数量是决策变量 → Cournot 族

这条判据写在剧本的 `fails_when` 里，由 [[playbook-composition]] 承载。
