/**
 * Significance testing for correlations.
 *
 * A correlation coefficient on its own says nothing about whether a
 * relationship is real. |r| = 0.5 is convincing across 30 days and worthless
 * across 5 -- the same number, opposite conclusions. And because we test every
 * pair of tracked things against every other, plus a lagged copy of each, we
 * run hundreds of tests per analysis. At a nominal 5% cutoff, hundreds of tests
 * against pure noise still produce a handful of "findings".
 *
 * So two things happen here: each correlation gets a p-value that accounts for
 * sample size, and the whole batch gets corrected for the number of tests run.
 */

/** Log-gamma, Lanczos approximation (g=7, n=9). Accurate to ~15 digits. */
function logGamma(x: number): number {
    const coefficients = [
        0.99999999999980993, 676.5203681218851, -1259.1392167224028,
        771.32342877765313, -176.61502916214059, 12.507343278686905,
        -0.13857109526572012, 9.9843695780195716e-6, 1.5056327351493116e-7,
    ];

    if (x < 0.5) {
        // Reflection formula, since the series below only converges for x >= 0.5
        return Math.log(Math.PI / Math.sin(Math.PI * x)) - logGamma(1 - x);
    }

    x -= 1;
    let a = coefficients[0];
    const t = x + 7.5;
    for (let i = 1; i < 9; i++) {
        a += coefficients[i] / (x + i);
    }

    return 0.5 * Math.log(2 * Math.PI) + (x + 0.5) * Math.log(t) - t + Math.log(a);
}

/**
 * Continued fraction for the incomplete beta function, evaluated with Lentz's
 * method. Converges quickly for x < (a+1)/(a+b+2); `incompleteBeta` flips the
 * arguments when it does not.
 */
function betaContinuedFraction(a: number, b: number, x: number): number {
    const MAX_ITERATIONS = 300;
    const EPSILON = 3e-16;
    const TINY = 1e-300;

    const qab = a + b;
    const qap = a + 1;
    const qam = a - 1;

    let c = 1;
    let d = 1 - (qab * x) / qap;
    if (Math.abs(d) < TINY) d = TINY;
    d = 1 / d;
    let result = d;

    for (let m = 1; m <= MAX_ITERATIONS; m++) {
        const m2 = 2 * m;

        let numerator = (m * (b - m) * x) / ((qam + m2) * (a + m2));
        d = 1 + numerator * d;
        if (Math.abs(d) < TINY) d = TINY;
        c = 1 + numerator / c;
        if (Math.abs(c) < TINY) c = TINY;
        d = 1 / d;
        result *= d * c;

        numerator = (-(a + m) * (qab + m) * x) / ((a + m2) * (qap + m2));
        d = 1 + numerator * d;
        if (Math.abs(d) < TINY) d = TINY;
        c = 1 + numerator / c;
        if (Math.abs(c) < TINY) c = TINY;
        d = 1 / d;

        const delta = d * c;
        result *= delta;

        if (Math.abs(delta - 1) < EPSILON) break;
    }

    return result;
}

/** Regularised incomplete beta function I_x(a, b). */
function incompleteBeta(a: number, b: number, x: number): number {
    if (x <= 0) return 0;
    if (x >= 1) return 1;

    const front = Math.exp(
        logGamma(a + b) - logGamma(a) - logGamma(b)
        + a * Math.log(x) + b * Math.log(1 - x)
    );

    return x < (a + 1) / (a + b + 2)
        ? (front * betaContinuedFraction(a, b, x)) / a
        : 1 - (front * betaContinuedFraction(b, a, 1 - x)) / b;
}

/**
 * Two-sided p-value for a Pearson correlation: the probability of seeing a
 * coefficient at least this extreme if the two things were unrelated.
 *
 * Returns 1 (no evidence at all) below three pairs, where the coefficient is
 * either undefined or trivially +/-1 and means nothing either way.
 */
export function pearsonPValue(coefficient: number, sampleSize: number): number {
    const df = sampleSize - 2;
    if (df < 1 || !Number.isFinite(coefficient)) return 1;

    const r = Math.min(1, Math.max(-1, coefficient));

    // A perfect correlation gives an infinite t; the limit of the p-value is 0.
    if (Math.abs(r) >= 1) return 0;

    const t = Math.abs(r) * Math.sqrt(df / (1 - r * r));
    return incompleteBeta(0.5 * df, 0.5, df / (df + t * t));
}

/**
 * The smallest |r| that would be significant at `alpha` for a given sample
 * size. Not used in the analysis itself -- it is for explaining to someone why
 * their 6 days of data cannot show them anything (|r| would have to exceed
 * 0.81), which is more useful than silently showing nothing.
 */
export function minimumDetectableCorrelation(sampleSize: number, alpha: number = 0.05): number {
    if (sampleSize < 3) return 1;

    let low = 0;
    let high = 1;
    for (let i = 0; i < 60; i++) {
        const mid = (low + high) / 2;
        if (pearsonPValue(mid, sampleSize) > alpha) {
            low = mid;
        } else {
            high = mid;
        }
    }

    return (low + high) / 2;
}

/**
 * Benjamini-Hochberg correction, returning a q-value per input p-value in the
 * original order.
 *
 * Bonferroni would be the simpler choice, but dividing by several hundred tests
 * leaves nothing visible short of a near-perfect relationship -- for a personal
 * tracking app that is the same as switching the feature off. Benjamini-Hochberg
 * instead controls the *proportion* of shown correlations that are false: at
 * q < 0.05, roughly one in twenty of what you are shown is expected to be
 * spurious, however many tests were run to find them.
 */
export function benjaminiHochberg(pValues: number[]): number[] {
    const total = pValues.length;
    if (total === 0) return [];

    const ordered = pValues
        .map((p, index) => ({p, index}))
        .sort((a, b) => a.p - b.p);

    const qValues = new Array<number>(total);

    // Walk from the largest p downwards, keeping a running minimum: q-values
    // must not decrease as p increases, and the step-up procedure is defined in
    // terms of the smallest adjusted value at or above each rank.
    let runningMinimum = 1;
    for (let rank = total; rank >= 1; rank--) {
        const {p, index} = ordered[rank - 1];
        runningMinimum = Math.min(runningMinimum, (p * total) / rank);
        // A q-value is never below its own p-value. That holds by definition,
        // but `p * total / rank` does not reproduce p exactly in floating point
        // at rank == total, so clamp rather than emit a q a hair below its p.
        qValues[index] = Math.min(1, Math.max(p, runningMinimum));
    }

    return qValues;
}
