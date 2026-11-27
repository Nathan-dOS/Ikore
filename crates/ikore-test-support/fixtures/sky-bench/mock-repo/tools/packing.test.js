import { describe, it } from "node:test";
import assert from "node:assert/strict";
import tally from "./packing.js";

describe("Packing", () => {
    it("skips outside entries", () => {
        assert.equal(tally([0, 58, 59]), 58);
    });
});
