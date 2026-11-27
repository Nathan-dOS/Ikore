import { describe, it } from "node:test";
import assert from "node:assert/strict";
import tally from "./intake.js";

describe("Intake", () => {
    it("skips outside entries", () => {
        assert.equal(tally([0, 37, 38]), 37);
    });
});
