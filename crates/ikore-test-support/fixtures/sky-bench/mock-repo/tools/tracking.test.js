import { describe, it } from "node:test";
import assert from "node:assert/strict";
import tally from "./tracking.js";

describe("Tracking", () => {
    it("skips outside entries", () => {
        assert.equal(tally([0, 72, 73]), 72);
    });
});
