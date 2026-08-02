//! Minimal dense-symmetric linear algebra for the spectral redundancy
//! functional (`redundancy::spectral_value`, redesign-tex
//! `spectral-bounded-slope.tex`).
//!
//! nrs-core is otherwise dependency-free by design, and the matrices
//! here are per-entry, per-sign-class cell kernels, so `score()`'s
//! production path uses the hand-rolled `householder_ql_eigen` below —
//! no linalg crate, nothing that could complicate a future wasm build.
//! `jacobi_eigen` was the original solver; it's kept as a slow-but-simple
//! correctness oracle for testing `householder_ql_eigen` against (see
//! that function's docs for why it replaced Jacobi in production), so
//! it's `#[cfg(test)]`-only now rather than dead code in release builds.
//!
//! A third option, `faer_eigen` (behind the opt-in `faer-eigen` feature),
//! was measured against both: on the real dataset's G-matrices — mostly
//! block-diagonal by anchor, since `k_feat` is unpopulated there — faer's
//! per-call overhead (thread-pool/parallelism check, scratch allocation,
//! SIMD kernel dispatch) outweighs its considerably better asymptotic
//! performance on genuinely large *dense* matrices, so it's a net
//! regression end-to-end despite winning by orders of magnitude on
//! synthetic dense random matrices of the same size. Left in as an
//! opt-in path (`redundancy::spectral_class` dispatches to it above a
//! size threshold when the feature is on) in case that tradeoff flips —
//! e.g. if `k_feat` becomes densely populated once `nrs-fit` lands.

#[cfg(test)]
/// The `(c, s)` of the Jacobi rotation that annihilates the `(p, q)`
/// entry of a symmetric matrix (Golub & Van Loan, Alg. 8.4.1
/// `sym.schur2`), given the `app`, `apq`, `aqq` block.
fn sym_schur2(app: f64, apq: f64, aqq: f64) -> (f64, f64) {
    if apq == 0.0 {
        return (1.0, 0.0);
    }
    let tau = (aqq - app) / (2.0 * apq);
    let t = if tau >= 0.0 {
        1.0 / (tau + (1.0 + tau * tau).sqrt())
    } else {
        -1.0 / (-tau + (1.0 + tau * tau).sqrt())
    };
    let c = 1.0 / (1.0 + t * t).sqrt();
    (c, t * c)
}

/// Eigen-decomposition of a real symmetric `n x n` matrix by cyclic
/// Jacobi rotation. Returns `(values, vectors)` where `values[k]` is the
/// k-th eigenvalue and `vectors[i][k]` is the i-th component of its unit
/// eigenvector — i.e. eigenvectors are the *columns* of `vectors`, so
/// `sum_i vectors[i][k]^2 == 1`. The input is consumed as the working
/// copy. No particular eigenvalue ordering is guaranteed.
///
/// Only the strict upper triangle needs to be meaningful on input; the
/// matrix is assumed symmetric. Convergence is quadratic; the sweep cap
/// is a safety net that small kernels never reach.
#[cfg(test)]
#[allow(clippy::needless_range_loop)] // paired (p, q) indices rotate rows/cols of two matrices
pub(crate) fn jacobi_eigen(mut a: Vec<Vec<f64>>) -> (Vec<f64>, Vec<Vec<f64>>) {
    let n = a.len();
    let mut v = vec![vec![0.0f64; n]; n];
    for (i, row) in v.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    if n <= 1 {
        let vals = (0..n).map(|i| a[i][i]).collect();
        return (vals, v);
    }

    // Convergence threshold on the (squared) off-diagonal Frobenius norm,
    // scaled by the matrix magnitude so it is meaningful at any scale.
    let mut scale = 1.0f64;
    for (i, row) in a.iter().enumerate() {
        for (j, &x) in row.iter().enumerate() {
            if i != j {
                scale = scale.max(x.abs());
            }
        }
    }
    let tol = (1e-15 * scale) * (1e-15 * scale);

    for _sweep in 0..100 {
        let mut off = 0.0;
        for p in 0..n {
            for q in (p + 1)..n {
                off += a[p][q] * a[p][q];
            }
        }
        if off <= tol {
            break;
        }
        for p in 0..n {
            for q in (p + 1)..n {
                if a[p][q] == 0.0 {
                    continue;
                }
                let (c, s) = sym_schur2(a[p][p], a[p][q], a[q][q]);
                // A <- A J  (rotate columns p, q).
                for row in a.iter_mut() {
                    let akp = row[p];
                    let akq = row[q];
                    row[p] = c * akp - s * akq;
                    row[q] = s * akp + c * akq;
                }
                // A <- J^T A  (rotate rows p, q); together this is the
                // similarity J^T A J, which zeroes the (p, q) entry.
                for k in 0..n {
                    let apk = a[p][k];
                    let aqk = a[q][k];
                    a[p][k] = c * apk - s * aqk;
                    a[q][k] = s * apk + c * aqk;
                }
                // V <- V J  (accumulate the eigenvectors).
                for row in v.iter_mut() {
                    let vkp = row[p];
                    let vkq = row[q];
                    row[p] = c * vkp - s * vkq;
                    row[q] = s * vkp + c * vkq;
                }
            }
        }
    }

    let vals = (0..n).map(|i| a[i][i]).collect();
    (vals, v)
}

/// Householder tridiagonalization (`tred2`) + implicit-shift QL
/// (`tql2`), the standard fast path for dense symmetric eigendecomposition
/// (Numerical Recipes §11.2-11.3 / EISPACK). `jacobi_eigen` does O(n^2)
/// rotations per sweep and needs several sweeps to converge, i.e. O(n^3)
/// work *per sweep*; this instead tridiagonalizes once in O(n^3) and
/// then extracts the spectrum from the tridiagonal form, which converges
/// in many fewer flops for the larger kernels the real dataset's biggest
/// franchise entries produce. Same `(values, vectors)` contract as
/// `jacobi_eigen` (columns are unit eigenvectors, no ordering
/// guaranteed), and kept alongside it (rather than replacing it) so the
/// two can be benchmarked and cross-checked against each other.
///
/// Ported from the classic EISPACK/Numerical Recipes routines, which are
/// 1-indexed throughout (loops like "i counts down from n to 2" don't
/// translate cleanly to 0-indexed `usize`); rather than re-derive the
/// index algebra and risk an off-by-one, the working matrix/vectors here
/// keep that 1-indexed layout internally (index 0 unused) and only
/// convert at the function boundary.
pub(crate) fn householder_ql_eigen(a_in: Vec<Vec<f64>>) -> (Vec<f64>, Vec<Vec<f64>>) {
    let n = a_in.len();
    if n <= 1 {
        let vals = (0..n).map(|i| a_in[i][i]).collect();
        let vecs = if n == 1 { vec![vec![1.0]] } else { Vec::new() };
        return (vals, vecs);
    }

    let mut z = vec![vec![0.0f64; n + 1]; n + 1];
    for i in 0..n {
        for j in 0..n {
            z[i + 1][j + 1] = a_in[i][j];
        }
    }
    let mut d = vec![0.0f64; n + 1];
    let mut e = vec![0.0f64; n + 1];
    tred2(&mut z, n, &mut d, &mut e);
    tql2(&mut z, n, &mut d, &mut e);

    let vals: Vec<f64> = d[1..=n].to_vec();
    let mut vecs = vec![vec![0.0f64; n]; n];
    for i in 0..n {
        for k in 0..n {
            vecs[i][k] = z[i + 1][k + 1];
        }
    }
    (vals, vecs)
}

/// Reduce the symmetric `z[1..=n][1..=n]` to tridiagonal form in place
/// via Householder reflections. On return, `d[1..=n]` holds the diagonal,
/// `e[2..=n]` the off-diagonal (`e[1]` unused pre-`tql2`), and `z` holds
/// the accumulated orthogonal transformation (so it can seed `tql2` to
/// recover eigenvectors of the *original* matrix, not just the
/// tridiagonal one).
#[allow(clippy::needless_range_loop)]
fn tred2(z: &mut [Vec<f64>], n: usize, d: &mut [f64], e: &mut [f64]) {
    for i in (2..=n).rev() {
        let l = i - 1;
        let mut h = 0.0f64;
        let mut scale = 0.0f64;
        if l > 1 {
            for k in 1..=l {
                scale += z[i][k].abs();
            }
            if scale == 0.0 {
                e[i] = z[i][l];
            } else {
                for k in 1..=l {
                    z[i][k] /= scale;
                    h += z[i][k] * z[i][k];
                }
                let mut f = z[i][l];
                let g = if f >= 0.0 { -h.sqrt() } else { h.sqrt() };
                e[i] = scale * g;
                h -= f * g;
                z[i][l] = f - g;
                f = 0.0;
                for j in 1..=l {
                    z[j][i] = z[i][j] / h;
                    let mut g2 = 0.0f64;
                    for k in 1..=j {
                        g2 += z[j][k] * z[i][k];
                    }
                    for k in (j + 1)..=l {
                        g2 += z[k][j] * z[i][k];
                    }
                    e[j] = g2 / h;
                    f += e[j] * z[i][j];
                }
                let hh = f / (h + h);
                for j in 1..=l {
                    let fj = z[i][j];
                    let gj = e[j] - hh * fj;
                    e[j] = gj;
                    for k in 1..=j {
                        z[j][k] -= fj * e[k] + gj * z[i][k];
                    }
                }
            }
        } else {
            e[i] = z[i][l];
        }
        d[i] = h;
    }
    d[1] = 0.0;
    e[1] = 0.0;
    for i in 1..=n {
        let l = i - 1;
        if d[i] != 0.0 {
            for j in 1..=l {
                let mut g = 0.0f64;
                for k in 1..=l {
                    g += z[i][k] * z[k][j];
                }
                for k in 1..=l {
                    z[k][j] -= g * z[k][i];
                }
            }
        }
        d[i] = z[i][i];
        z[i][i] = 1.0;
        for j in 1..=l {
            z[j][i] = 0.0;
            z[i][j] = 0.0;
        }
    }
}

/// Diagonalize the tridiagonal `(d, e)` pair via implicit-shift QL with
/// implicit shifts, accumulating rotations into `z` (seeded by `tred2`'s
/// transformation) so it ends up holding the original matrix's
/// eigenvectors. `d` becomes the eigenvalues on return.
fn tql2(z: &mut [Vec<f64>], n: usize, d: &mut [f64], e: &mut [f64]) {
    for i in 2..=n {
        e[i - 1] = e[i];
    }
    e[n] = 0.0;
    for l in 1..=n {
        let mut iter = 0;
        loop {
            let mut m = n;
            for mm in l..n {
                let dd = d[mm].abs() + d[mm + 1].abs();
                if (e[mm].abs() + dd) == dd {
                    m = mm;
                    break;
                }
            }
            if m == l {
                break;
            }
            iter += 1;
            if iter > 50 {
                // Defensive only: the kernels this crate diagonalizes are
                // small, symmetric, and PSD-clipped, so this shouldn't
                // trigger in practice. Bail with whatever precision was
                // reached rather than loop forever or panic on live
                // scoring traffic.
                break;
            }
            let mut g = (d[l + 1] - d[l]) / (2.0 * e[l]);
            let mut r = g.hypot(1.0);
            g = d[m] - d[l] + e[l] / (g + r.copysign(g));
            let mut s = 1.0f64;
            let mut c = 1.0f64;
            let mut p = 0.0f64;
            let mut ii = m - 1;
            let mut zeroed = false;
            loop {
                let f = s * e[ii];
                let b = c * e[ii];
                r = f.hypot(g);
                e[ii + 1] = r;
                if r == 0.0 {
                    d[ii + 1] -= p;
                    e[m] = 0.0;
                    zeroed = true;
                    break;
                }
                s = f / r;
                c = g / r;
                g = d[ii + 1] - p;
                r = (d[ii] - g) * s + 2.0 * c * b;
                p = s * r;
                d[ii + 1] = g + p;
                g = c * r - b;
                for k in 1..=n {
                    let f2 = z[k][ii + 1];
                    z[k][ii + 1] = s * z[k][ii] + c * f2;
                    z[k][ii] = c * z[k][ii] - s * f2;
                }
                if ii == l {
                    break;
                }
                ii -= 1;
            }
            if zeroed {
                continue;
            }
            d[l] -= p;
            e[l] = g;
            e[m] = 0.0;
        }
    }
}

/// `faer`'s dense self-adjoint eigensolver, for perf comparison against
/// `jacobi_eigen`/`householder_ql_eigen` (see linalg.rs's header on why
/// nrs-core doesn't take this as a real dependency by default). Same
/// `(values, vectors)` contract as the other two.
#[cfg(feature = "faer-eigen")]
pub(crate) fn faer_eigen(a_in: Vec<Vec<f64>>) -> (Vec<f64>, Vec<Vec<f64>>) {
    use faer::{Mat, Side};

    let n = a_in.len();
    if n == 0 {
        return (Vec::new(), Vec::new());
    }
    let a = Mat::from_fn(n, n, |i, j| a_in[i][j]);
    let eig = a.self_adjoint_eigen(Side::Lower).expect("symmetric input");
    let vals: Vec<f64> = (0..n).map(|i| eig.S()[i]).collect();
    let u = eig.U();
    let vecs: Vec<Vec<f64>> = (0..n).map(|i| (0..n).map(|k| u[(i, k)]).collect()).collect();
    (vals, vecs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reconstruct `A` from `V diag(lambda) V^T` and check it matches —
    /// the eigenpairs are correct iff this round-trips.
    fn reconstruct(vals: &[f64], vecs: &[Vec<f64>]) -> Vec<Vec<f64>> {
        let n = vals.len();
        let mut out = vec![vec![0.0f64; n]; n];
        for (i, out_row) in out.iter_mut().enumerate() {
            for (j, cell) in out_row.iter_mut().enumerate() {
                *cell = (0..n).map(|k| vals[k] * vecs[i][k] * vecs[j][k]).sum();
            }
        }
        out
    }

    #[test]
    fn diagonal_matrix_is_its_own_spectrum() {
        let a = vec![vec![3.0, 0.0], vec![0.0, 5.0]];
        let (vals, _) = jacobi_eigen(a);
        let mut sorted = vals.clone();
        sorted.sort_by(|x, y| x.partial_cmp(y).unwrap());
        assert!((sorted[0] - 3.0).abs() < 1e-12);
        assert!((sorted[1] - 5.0).abs() < 1e-12);
    }

    #[test]
    fn known_2x2_symmetric() {
        // [[2,1],[1,2]] has eigenvalues 1 and 3.
        let a = vec![vec![2.0, 1.0], vec![1.0, 2.0]];
        let (vals, vecs) = jacobi_eigen(a.clone());
        let mut sorted = vals.clone();
        sorted.sort_by(|x, y| x.partial_cmp(y).unwrap());
        assert!((sorted[0] - 1.0).abs() < 1e-10, "{sorted:?}");
        assert!((sorted[1] - 3.0).abs() < 1e-10, "{sorted:?}");
        // Eigenvectors are unit and reconstruct A.
        #[allow(clippy::needless_range_loop)] // k is a column index into vecs
        for k in 0..2 {
            let norm: f64 = (0..2).map(|i| vecs[i][k] * vecs[i][k]).sum();
            assert!((norm - 1.0).abs() < 1e-10);
        }
        let r = reconstruct(&vals, &vecs);
        for i in 0..2 {
            for j in 0..2 {
                assert!((r[i][j] - a[i][j]).abs() < 1e-10, "{i},{j}: {}", r[i][j]);
            }
        }
    }

    #[test]
    fn reconstructs_a_dense_3x3() {
        let a = vec![
            vec![4.0, 1.0, -2.0],
            vec![1.0, 2.0, 0.5],
            vec![-2.0, 0.5, 3.0],
        ];
        let (vals, vecs) = jacobi_eigen(a.clone());
        // Trace is preserved.
        let tr: f64 = vals.iter().sum();
        assert!((tr - 9.0).abs() < 1e-9, "trace {tr}");
        let r = reconstruct(&vals, &vecs);
        for i in 0..3 {
            for j in 0..3 {
                assert!((r[i][j] - a[i][j]).abs() < 1e-9, "{i},{j}");
            }
        }
    }

    // ---- householder_ql_eigen: same contract, cross-checked against
    // jacobi_eigen (the exhaustively-tested reference) rather than
    // duplicating every hand-worked example. ----

    #[test]
    fn householder_ql_matches_jacobi_on_hand_worked_cases() {
        let cases: Vec<Vec<Vec<f64>>> = vec![
            vec![vec![3.0, 0.0], vec![0.0, 5.0]],
            vec![vec![2.0, 1.0], vec![1.0, 2.0]],
            vec![
                vec![4.0, 1.0, -2.0],
                vec![1.0, 2.0, 0.5],
                vec![-2.0, 0.5, 3.0],
            ],
        ];
        for a in cases {
            let (mut vals_j, _) = jacobi_eigen(a.clone());
            let (vals_h, vecs_h) = householder_ql_eigen(a.clone());
            let mut vals_h_sorted = vals_h.clone();
            vals_j.sort_by(|x, y| x.partial_cmp(y).unwrap());
            vals_h_sorted.sort_by(|x, y| x.partial_cmp(y).unwrap());
            for (vj, vh) in vals_j.iter().zip(&vals_h_sorted) {
                assert!((vj - vh).abs() < 1e-8, "{a:?}: jacobi {vals_j:?} vs householder {vals_h_sorted:?}");
            }
            let r = reconstruct(&vals_h, &vecs_h);
            let n = a.len();
            for i in 0..n {
                for j in 0..n {
                    assert!((r[i][j] - a[i][j]).abs() < 1e-8, "{a:?} {i},{j}: {}", r[i][j]);
                }
            }
        }
    }

    #[test]
    fn householder_ql_handles_trivial_sizes() {
        let (vals0, vecs0) = householder_ql_eigen(Vec::new());
        assert!(vals0.is_empty() && vecs0.is_empty());

        let (vals1, vecs1) = householder_ql_eigen(vec![vec![7.0]]);
        assert_eq!(vals1, vec![7.0]);
        assert_eq!(vecs1, vec![vec![1.0]]);
    }

    /// Cross-check against `jacobi_eigen` (already reconstruction-tested
    /// above) across many random symmetric matrices, including sizes
    /// large enough to exercise the QL shift loop's iteration more than
    /// once — the hand-worked cases above are all tiny.
    #[test]
    fn householder_ql_matches_jacobi_on_random_matrices() {
        // A small xorshift so the test has no extra dependency and is
        // still fully deterministic across runs.
        let mut state: u64 = 0x2545F4914F6CDD1D;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            // Map to [-1, 1].
            ((state >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
        };

        for &n in &[1usize, 2, 3, 5, 8, 16, 40] {
            let mut a = vec![vec![0.0f64; n]; n];
            for i in 0..n {
                for j in i..n {
                    let x = next();
                    a[i][j] = x;
                    a[j][i] = x;
                }
            }
            let (mut vals_j, _) = jacobi_eigen(a.clone());
            let (vals_h, vecs_h) = householder_ql_eigen(a.clone());
            vals_j.sort_by(|x, y| x.partial_cmp(y).unwrap());
            let mut vals_h_sorted = vals_h.clone();
            vals_h_sorted.sort_by(|x, y| x.partial_cmp(y).unwrap());
            for (vj, vh) in vals_j.iter().zip(&vals_h_sorted) {
                assert!(
                    (vj - vh).abs() < 1e-6 * (1.0 + vj.abs()),
                    "n={n}: jacobi {vals_j:?} vs householder {vals_h_sorted:?}"
                );
            }
            let r = reconstruct(&vals_h, &vecs_h);
            for i in 0..n {
                for j in 0..n {
                    assert!(
                        (r[i][j] - a[i][j]).abs() < 1e-6,
                        "n={n} {i},{j}: got {} want {}",
                        r[i][j],
                        a[i][j]
                    );
                }
            }

            #[cfg(feature = "faer-eigen")]
            {
                let (vals_f, vecs_f) = faer_eigen(a.clone());
                let mut vals_f_sorted = vals_f.clone();
                vals_f_sorted.sort_by(|x, y| x.partial_cmp(y).unwrap());
                for (vj, vf) in vals_j.iter().zip(&vals_f_sorted) {
                    assert!(
                        (vj - vf).abs() < 1e-6 * (1.0 + vj.abs()),
                        "n={n}: jacobi {vals_j:?} vs faer {vals_f_sorted:?}"
                    );
                }
                let rf = reconstruct(&vals_f, &vecs_f);
                for i in 0..n {
                    for j in 0..n {
                        assert!(
                            (rf[i][j] - a[i][j]).abs() < 1e-6,
                            "faer n={n} {i},{j}: got {} want {}",
                            rf[i][j],
                            a[i][j]
                        );
                    }
                }
            }
        }
    }

    /// Not a correctness test: prints raw eigensolver-only timings for
    /// `jacobi_eigen` vs `householder_ql_eigen` at sizes matching the
    /// real dataset's biggest per-entry kernels (see the score_bench
    /// perf investigation). `cargo test -p nrs-core --release
    /// householder_ql_vs_jacobi_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn householder_ql_vs_jacobi_timing() {
        use std::time::Instant;

        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
        };

        for &n in &[1usize, 2, 4, 8, 16, 32, 50, 100, 250, 509] {
            let mut a = vec![vec![0.0f64; n]; n];
            for i in 0..n {
                for j in i..n {
                    let x = next();
                    a[i][j] = x;
                    a[j][i] = x;
                }
            }

            let reps = if n <= 16 {
                20_000
            } else if n <= 100 {
                20
            } else {
                5
            };

            let t0 = Instant::now();
            for _ in 0..reps {
                std::hint::black_box(jacobi_eigen(a.clone()));
            }
            let jacobi_ms = t0.elapsed().as_secs_f64() * 1e3 / reps as f64;

            let t1 = Instant::now();
            for _ in 0..reps {
                std::hint::black_box(householder_ql_eigen(a.clone()));
            }
            let householder_ms = t1.elapsed().as_secs_f64() * 1e3 / reps as f64;

            #[cfg(feature = "faer-eigen")]
            let faer_ms = {
                let t2 = Instant::now();
                for _ in 0..reps {
                    std::hint::black_box(faer_eigen(a.clone()));
                }
                t2.elapsed().as_secs_f64() * 1e3 / reps as f64
            };
            #[cfg(not(feature = "faer-eigen"))]
            let faer_ms = f64::NAN;

            println!(
                "n={n:4}  jacobi={jacobi_ms:8.3}ms  householder_ql={householder_ms:8.3}ms  faer={faer_ms:8.3}ms  speedup(hh/jacobi)={:.2}x  speedup(faer/jacobi)={:.2}x",
                jacobi_ms / householder_ms,
                jacobi_ms / faer_ms
            );
        }
    }
}
