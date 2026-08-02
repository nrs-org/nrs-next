//! Minimal dense-symmetric linear algebra for the spectral redundancy
//! functional (`redundancy::spectral_value`, redesign-tex
//! `spectral-bounded-slope.tex`).
//!
//! nrs-core is otherwise dependency-free by design, and the matrices
//! here are per-entry, per-sign-class cell kernels (small `n`), so a
//! hand-rolled cyclic Jacobi eigensolver is both sufficient and keeps
//! the zero-dep contract — no linalg crate, nothing that could
//! complicate a future wasm build. Jacobi is also famously accurate on
//! the *small* eigenvalues a redundancy discount cares about: unlike a
//! tridiagonal-QR path it computes them to high relative accuracy rather
//! than losing the tail modes to cancellation.

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
}
