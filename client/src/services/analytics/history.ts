import type {CorrelationResult} from "@perfice/services/analytics/analytics";
import {parseJsonFromLocalStorage} from "@perfice/util/local";

const HISTORY_STORE_KEY = "correlations_history";

export interface AnalyticsHistoryEntry {
    key: string;
    coefficient: number;
    timestamp: number;

    // Absent on entries stored before significance testing existed.
    pValue?: number;
    qValue?: number;
    sampleSize?: number;
}

/**
 * Default false discovery rate: of the correlations shown, roughly one in
 * twenty is expected to be a coincidence. Stricter than that and a personal
 * dataset -- a few dozen days, a handful of tracked things -- shows nothing at
 * all; looser and the list fills with noise.
 */
export const DEFAULT_SIGNIFICANCE_LEVEL = 0.05;

export class AnalyticsHistoryService {

    private entries: AnalyticsHistoryEntry[] = [];
    private readonly confidenceThreshold: number;
    private readonly changeThreshold: number;
    private readonly significanceLevel: number;

    constructor(confidenceThreshold: number, changeThreshold: number,
                significanceLevel: number = DEFAULT_SIGNIFICANCE_LEVEL) {
        this.confidenceThreshold = confidenceThreshold;
        this.changeThreshold = changeThreshold;
        this.significanceLevel = significanceLevel;
    }

    load() {
        this.entries = parseJsonFromLocalStorage(HISTORY_STORE_KEY) ?? [];
    }

    getHistoryByKey(key: string): AnalyticsHistoryEntry | undefined {
        return this.entries.find(e => e.key == key);
    }

    getNewestCorrelations(limit: number, until: number): AnalyticsHistoryEntry[] {
        return this.entries
            .filter(e => e.timestamp <= until)
            .sort((a, b) => {
                // Mainly sort by timestamp, but if timestamps are the same, sort by coefficient
                if (a.timestamp == b.timestamp) {
                    return Math.abs(b.coefficient) - Math.abs(a.coefficient);
                }

                return b.timestamp - a.timestamp;
            })
            .slice(0, limit);
    }

    processResult(correlations: Map<string, CorrelationResult>, date: Date) {
        let newTimestamp = date.getTime();

        // Keyed by correlation key so entries that did not appear in this run
        // survive. Replacing the list outright would drop the history of any
        // correlation that dipped below the threshold today, so when it came
        // back it would look like a brand new discovery -- and coefficients
        // drift across the threshold constantly. That also silently defeated
        // the changeThreshold check below, which exists to stop exactly this.
        let merged: Map<string, AnalyticsHistoryEntry> = new Map(
            this.entries.map(e => [e.key, e])
        );

        for (let [key, correlation] of correlations.entries()) {
            if (!this.isSignificant(correlation)) {
                // Ran the test and it did not hold up this time. Forget it, so
                // that if it returns later it is genuinely news again.
                merged.delete(key);
                continue;
            }

            let timestamp: number = newTimestamp;
            let existing = merged.get(key);
            // If the change in coefficient was large, consider it a "new" correlation (i.e don't use previous timestamp)
            if (existing != null && !(Math.abs(existing.coefficient - correlation.coefficient) > this.changeThreshold)) {
                timestamp = existing.timestamp;
            }

            merged.set(key, {
                key,
                coefficient: correlation.coefficient,
                timestamp,
                pValue: correlation.pValue,
                qValue: correlation.qValue,
                sampleSize: correlation.sampleSize
            });
        }

        let result = [...merged.values()];
        localStorage.setItem(HISTORY_STORE_KEY, JSON.stringify(result));
        this.entries = result;
    }

    /**
     * Whether a correlation is worth showing at all.
     *
     * Both halves matter. `qValue` asks whether the relationship is likely to be
     * real given how much data there is and how many pairs were tested;
     * `confidenceThreshold` asks whether it is large enough to care about. With
     * enough days a trivial relationship becomes statistically detectable, and
     * telling someone about an r of 0.08 is noise of a different kind.
     */
    private isSignificant(correlation: CorrelationResult): boolean {
        return correlation.qValue < this.significanceLevel
            && Math.abs(correlation.coefficient) >= this.confidenceThreshold;
    }

    getAllHistory(): AnalyticsHistoryEntry[] {
        return this.entries;
    }

    importHistory(data: AnalyticsHistoryEntry[]) {
        this.entries = data;
        localStorage.setItem(HISTORY_STORE_KEY, JSON.stringify(data));
    }

}