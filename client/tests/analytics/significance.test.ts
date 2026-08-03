import {describe, expect, test} from "vitest";
import {
    benjaminiHochberg,
    minimumDetectableCorrelation,
    pearsonPValue
} from "../../src/services/analytics/significance";

describe("pearsonPValue", () => {
    // Critical values of r at alpha = 0.05 (two-tailed), from standard tables.
    // Checking against published numbers rather than against our own output --
    // otherwise the test only proves the code agrees with itself.
    test.each([
        [5, 0.878],
        [7, 0.754],
        [10, 0.632],
        [12, 0.576],
        [20, 0.444],
        [30, 0.361],
        [50, 0.279],
    ])("critical r at n=%i is %f", (n, expected) => {
        expect(pearsonPValue(expected, n)).toBeCloseTo(0.05, 3);
    });

    test("the same coefficient means less on less data", () => {
        // This is the whole point: r = 0.5 is noise at n = 5 and solid at n = 30.
        expect(pearsonPValue(0.5, 5)).toBeCloseTo(0.391, 3);
        expect(pearsonPValue(0.5, 10)).toBeCloseTo(0.141, 3);
        expect(pearsonPValue(0.5, 20)).toBeCloseTo(0.025, 3);
        expect(pearsonPValue(0.5, 30)).toBeCloseTo(0.005, 3);
    });

    test("sign does not matter", () => {
        expect(pearsonPValue(-0.6, 15)).toBeCloseTo(pearsonPValue(0.6, 15), 12);
    });

    test("no correlation is maximally unremarkable", () => {
        expect(pearsonPValue(0, 30)).toBeCloseTo(1, 12);
    });

    test("a perfect correlation has p = 0", () => {
        expect(pearsonPValue(1, 10)).toBe(0);
        expect(pearsonPValue(-1, 10)).toBe(0);
    });

    test("too few points is no evidence, not strong evidence", () => {
        // Two points are always perfectly collinear. Reporting p = 0 there would
        // make the emptiest possible dataset look like the strongest finding.
        expect(pearsonPValue(1, 2)).toBe(1);
        expect(pearsonPValue(0.9, 1)).toBe(1);
        expect(pearsonPValue(0.9, 0)).toBe(1);
    });

    test("survives degenerate input", () => {
        expect(pearsonPValue(NaN, 20)).toBe(1);
        expect(pearsonPValue(1.0000001, 20)).toBe(0);
    });

    test("p decreases monotonically as the correlation strengthens", () => {
        let previous = 1;
        for (const r of [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9]) {
            const p = pearsonPValue(r, 25);
            expect(p).toBeLessThan(previous);
            previous = p;
        }
    });
});

describe("minimumDetectableCorrelation", () => {
    test("matches the critical values of r", () => {
        expect(minimumDetectableCorrelation(5)).toBeCloseTo(0.878, 3);
        expect(minimumDetectableCorrelation(10)).toBeCloseTo(0.632, 3);
        expect(minimumDetectableCorrelation(30)).toBeCloseTo(0.361, 3);
    });

    test("more data detects weaker relationships", () => {
        expect(minimumDetectableCorrelation(30)).toBeLessThan(minimumDetectableCorrelation(10));
    });

    test("nothing is detectable below three points", () => {
        expect(minimumDetectableCorrelation(2)).toBe(1);
    });
});

describe("benjaminiHochberg", () => {
    test("worked example", () => {
        // Benjamini & Hochberg (1995), the p-values from their Table 1.
        const p = [0.0001, 0.0004, 0.0019, 0.0095, 0.0201, 0.0278, 0.0298,
            0.0344, 0.0459, 0.3240, 0.4262, 0.5719, 0.6528, 0.7590, 1.0];
        const q = benjaminiHochberg(p);

        // At q < 0.05 the first four are rejected, matching the paper.
        expect(q.filter(v => v < 0.05)).toHaveLength(4);
        expect(q[0]).toBeCloseTo(0.0015, 4);
        expect(q[3]).toBeCloseTo(0.0356, 4);
    });

    test("returns q-values in the original order", () => {
        const q = benjaminiHochberg([0.5, 0.01, 0.2]);
        expect(q[1]).toBeLessThan(q[2]);
        expect(q[2]).toBeLessThan(q[0]);
    });

    test("q is never below p, and never above 1", () => {
        const p = [0.001, 0.01, 0.04, 0.3, 0.8, 0.99];
        const q = benjaminiHochberg(p);
        q.forEach((value, i) => {
            expect(value).toBeGreaterThanOrEqual(p[i]);
            expect(value).toBeLessThanOrEqual(1);
        });
    });

    test("q never decreases as p increases", () => {
        const p = [0.001, 0.002, 0.003, 0.02, 0.04, 0.5, 0.9];
        const q = benjaminiHochberg(p);
        for (let i = 1; i < q.length; i++) {
            expect(q[i]).toBeGreaterThanOrEqual(q[i - 1]);
        }
    });

    test("a single test is left alone", () => {
        expect(benjaminiHochberg([0.03])).toEqual([0.03]);
    });

    test("many tests against noise yield nothing", () => {
        // 200 uniformly spread p-values is what pure noise looks like. None of
        // them should survive -- this is the case the correction exists for.
        const p = Array.from({length: 200}, (_, i) => (i + 1) / 200);
        expect(benjaminiHochberg(p).filter(v => v < 0.05)).toHaveLength(0);
    });

    test("a real effect still survives being buried in noise", () => {
        // One genuine finding among 199 null results must not be discarded, or
        // the correction has simply switched the feature off.
        const p = [0.00001, ...Array.from({length: 199}, (_, i) => 0.05 + (i / 199) * 0.95)];
        expect(benjaminiHochberg(p)[0]).toBeLessThan(0.05);
    });

    test("empty input", () => {
        expect(benjaminiHochberg([])).toEqual([]);
    });
});
