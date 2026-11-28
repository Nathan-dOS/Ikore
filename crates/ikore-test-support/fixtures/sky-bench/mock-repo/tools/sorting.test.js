import { describe, it } from "node:test";
import assert from "node:assert/strict";
import tally from "./sorting.js";

describe("Sorting", () => {
    it("skips outside entries", () => {
        assert.equal(tally([0, 51, 52]), 51);
    });
});
