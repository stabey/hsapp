//! 确定性 RNG(splitmix64)。M3 不加 `rand` 依赖,同 seed 必须逐字节可复现。

/// SplitMix64:64 位可复现伪随机。
pub(crate) struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_5893);
        z ^ (z >> 31)
    }

    /// 半开区间 `[0, 1)`。
    pub(crate) fn next_unit(&mut self) -> f64 {
        // 取高 53 位,与 IEEE-754 双精度尾数对齐,避免 u64::MAX as f64 精度丢失。
        let mantissa = self.next_u64() >> 11;
        (mantissa as f64) / ((1u64 << 53) as f64)
    }
}

/// `C(n, k)`;k>n 为 0;溢出返回 None。
pub(crate) fn checked_comb(n: u64, k: u64) -> Option<u64> {
    if k > n {
        return Some(0);
    }
    let k = k.min(n - k);
    let mut result = 1u64;
    for i in 0..k {
        result = result.checked_mul(n - i)?.checked_div(i + 1)?;
    }
    Some(result)
}

/// 按字典序生成 `[0, n)` 的 k-组合。
pub(crate) fn k_combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    if k == 0 {
        out.push(Vec::new());
        return out;
    }
    if k > n {
        return out;
    }
    let mut current = Vec::with_capacity(k);
    rec_combinations(0, n, k, &mut current, &mut out);
    out
}

fn rec_combinations(
    start: usize,
    n: usize,
    k: usize,
    current: &mut Vec<usize>,
    out: &mut Vec<Vec<usize>>,
) {
    if current.len() == k {
        out.push(current.clone());
        return;
    }
    let need = k - current.len();
    let last = n - need;
    for i in start..=last {
        current.push(i);
        rec_combinations(i + 1, n, k, current, out);
        current.pop();
    }
}

/// 无放回均匀抽 k 个下标,返回升序下标(表示确定)。
pub(crate) fn sample_combination(n: usize, k: usize, rng: &mut SplitMix64) -> Vec<usize> {
    debug_assert!(k <= n);
    let mut idx: Vec<usize> = (0..n).collect();
    for i in 0..k {
        let remain = n - i;
        let pick = (rng.next_u64() as usize) % remain;
        idx.swap(i, i + pick);
    }
    let mut chosen = idx[..k].to_vec();
    chosen.sort_unstable();
    chosen
}

/// 按权重抽一个下标;权重全 0 时退回最后一个。
pub(crate) fn sample_weighted(weights: &[f64], rng: &mut SplitMix64) -> usize {
    let total: f64 = weights.iter().copied().sum();
    if total <= 0.0 {
        return weights.len().saturating_sub(1);
    }
    let mut needle = rng.next_unit() * total;
    for (i, weight) in weights.iter().enumerate() {
        if needle < *weight {
            return i;
        }
        needle -= *weight;
    }
    weights.len().saturating_sub(1)
}
