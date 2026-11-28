import { describe, it } from "node:test";
import assert from "node:assert/strict";
import tally from "./weighing.js";

describe("Weighing", () => {
    it("skips outside entries", () => {
        assert.equal(tally([0, 44, 45]), 44);
    });
});
