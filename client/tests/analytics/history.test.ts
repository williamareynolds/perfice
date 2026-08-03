import {beforeEach, expect, test} from "vitest";
import {
    DummyFormService,
    DummyJournalCollection,
    DummyTagCollection,
    DummyTagEntryCollection
} from "../dummy-collections";
import {mockEntry, mockForm} from "./raw.test";
import {pNumber} from "../../src/model/primitive/primitive";
import {AnalyticsHistoryService} from "../../src/services/analytics/history";
import {AnalyticsService} from "../../src/services/analytics/analytics";
import {FormQuestionDataType} from "../../src/model/form/form";
import {SimpleTimeScopeType, WeekStart} from "../../src/model/variable/time/time";
import {AnalyticsSettings} from "../../src/model/analytics/analytics";
import type {JournalEntry} from "../../src/model/journal/journal";

const KEY = "test_form:test|test_form2:test";

// Long enough for a correlation to mean something. The earlier version of these
// tests ran on three to five days, where |r| = 0.55 has a p-value around 0.34 --
// the coefficients they asserted on were arithmetic performed on noise.
const DAYS = 24;

function mockAnalyticsSettings(): AnalyticsSettings[] {
    return [
        {id: "test_form", questionId: "test", useMeanValue: {"test": true}, interpolate: false},
        {id: "test_form2", questionId: "test", useMeanValue: {"test": true}, interpolate: false}
    ];
}

/** One entry per day, starting 1970-01-01. */
function series(formId: string, values: number[]): JournalEntry[] {
    return values.map((value, i) =>
        mockEntry(formId, {"test": pNumber(value)}, new Date(1970, 0, 1 + i).getTime()));
}

function analyticsService(journal: DummyJournalCollection): AnalyticsService {
    return new AnalyticsService(
        new DummyFormService([
            mockForm("test_form", {"test": FormQuestionDataType.NUMBER}),
            mockForm("test_form2", {"test": FormQuestionDataType.NUMBER}),
        ]),
        journal,
        new DummyTagCollection([]),
        new DummyTagEntryCollection([]),
        WeekStart.MONDAY
    );
}

async function correlate(analytics: AnalyticsService, date: Date, range: number) {
    let [forms, entries] = await analytics.fetchFormsAndEntries(date, range);
    let [rawValues] = await analytics.constructRawValues(forms, entries, SimpleTimeScopeType.DAILY);
    let [tagValues] = await analytics.fetchTagValues(SimpleTimeScopeType.DAILY, date, range);
    return analytics.runBasicCorrelations(rawValues, tagValues, mockAnalyticsSettings(),
        date, range, 10);
}

/** Values that track each other closely, with enough spread to be measurable. */
function strongPair(): [number[], number[]] {
    const first = Array.from({length: DAYS}, (_, i) => 10 + (i % 8) * 2);
    const second = first.map((v, i) => v + (i % 3) - 1);
    return [first, second];
}

beforeEach(() => {
    localStorage.clear();
});

test("stores a correlation that clears significance", async () => {
    const [first, second] = strongPair();
    const journal = new DummyJournalCollection([
        ...series("test_form", first),
        ...series("test_form2", second),
    ]);

    const date = new Date(1970, 0, 1 + DAYS);
    const results = await correlate(analyticsService(journal), date, DAYS);

    const stored = results.get(KEY)!;
    expect(stored).toBeDefined();
    expect(stored.sampleSize).toBeGreaterThanOrEqual(20);
    expect(stored.qValue).toBeLessThan(0.05);

    const history = new AnalyticsHistoryService(0.5, 0.3);
    history.processResult(results, date);

    const reloaded = new AnalyticsHistoryService(0.5, 0.3);
    reloaded.load();
    const entry = reloaded.getAllHistory().find(e => e.key == KEY);
    expect(entry).toMatchObject({key: KEY, timestamp: date.getTime()});
    expect(entry!.pValue).toBeLessThan(0.05);
});

test("a coefficient that cannot clear significance is not stored", async () => {
    // Unrelated series. Whatever coefficient falls out of 24 days of this, it
    // is not a finding, and nothing should reach the history.
    const journal = new DummyJournalCollection([
        ...series("test_form", Array.from({length: DAYS}, (_, i) => 10 + (i % 5))),
        ...series("test_form2", Array.from({length: DAYS}, (_, i) => 10 + (i % 7 < 3 ? 3 : 0))),
    ]);

    const date = new Date(1970, 0, 1 + DAYS);
    const results = await correlate(analyticsService(journal), date, DAYS);

    const history = new AnalyticsHistoryService(0.5, 0.3);
    history.processResult(results, date);

    for (const entry of history.getAllHistory()) {
        const result = results.get(entry.key)!;
        expect(result.qValue).toBeLessThan(0.05);
    }
});

test("timestamp is preserved when the coefficient barely moves", async () => {
    const [first, second] = strongPair();
    const journal = new DummyJournalCollection([
        ...series("test_form", first),
        ...series("test_form2", second),
    ]);
    const analytics = analyticsService(journal);
    const history = new AnalyticsHistoryService(0.5, 0.3);

    const firstDate = new Date(1970, 0, 1 + DAYS);
    history.processResult(await correlate(analytics, firstDate, DAYS), firstDate);

    // A day later with one more consistent point: same relationship, so this is
    // not a new discovery and must keep its original timestamp.
    journal.createEntry(mockEntry("test_form", {"test": pNumber(10)},
        new Date(1970, 0, 1 + DAYS).getTime()));
    journal.createEntry(mockEntry("test_form2", {"test": pNumber(10)},
        new Date(1970, 0, 1 + DAYS).getTime()));

    const secondDate = new Date(1970, 0, 2 + DAYS);
    history.processResult(await correlate(analytics, secondDate, DAYS + 1), secondDate);

    const entry = history.getAllHistory().find(e => e.key == KEY);
    expect(entry?.timestamp).toBe(firstDate.getTime());
});

test("history survives a correlation that is not re-tested", async () => {
    // The bug this replaces: processResult rebuilt the list from scratch every
    // run, so anything absent from the current results lost its history. When it
    // reappeared it was reported as a brand new discovery, over and over.
    const [first, second] = strongPair();
    const journal = new DummyJournalCollection([
        ...series("test_form", first),
        ...series("test_form2", second),
    ]);

    const date = new Date(1970, 0, 1 + DAYS);
    const history = new AnalyticsHistoryService(0.5, 0.3);
    history.processResult(await correlate(analyticsService(journal), date, DAYS), date);
    expect(history.getAllHistory().find(e => e.key == KEY)).toBeDefined();

    // A later run that tested nothing at all -- too little data that day, say.
    const laterDate = new Date(1970, 0, 8 + DAYS);
    history.processResult(new Map(), laterDate);

    const entry = history.getAllHistory().find(e => e.key == KEY);
    expect(entry).toBeDefined();
    expect(entry!.timestamp).toBe(date.getTime());

    // And it is still there after a reload, not just in memory.
    const reloaded = new AnalyticsHistoryService(0.5, 0.3);
    reloaded.load();
    expect(reloaded.getAllHistory().find(e => e.key == KEY)?.timestamp).toBe(date.getTime());
});

test("a correlation that is tested and fails is forgotten", async () => {
    // The other half: absent from the results means "not measured", but present
    // and insignificant means "measured, did not hold up". Only the second
    // should clear the entry, so that a genuine return counts as news again.
    const [first, second] = strongPair();
    const journal = new DummyJournalCollection([
        ...series("test_form", first),
        ...series("test_form2", second),
    ]);

    const date = new Date(1970, 0, 1 + DAYS);
    const results = await correlate(analyticsService(journal), date, DAYS);
    const history = new AnalyticsHistoryService(0.5, 0.3);
    history.processResult(results, date);
    expect(history.getAllHistory().find(e => e.key == KEY)).toBeDefined();

    const failed = new Map(results);
    failed.set(KEY, {...results.get(KEY)!, coefficient: 0.05, pValue: 0.8, qValue: 0.9});

    const laterDate = new Date(1970, 0, 8 + DAYS);
    history.processResult(failed, laterDate);
    expect(history.getAllHistory().find(e => e.key == KEY)).toBeUndefined();
});
