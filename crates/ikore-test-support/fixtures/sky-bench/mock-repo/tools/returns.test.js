import { describe, it } from "node:test";
import assert from "node:assert/strict";
import tally from "./returns.js";

describe("Returns", () => {
    it("skips outside entries", () => {
        assert.equal(tally([0, 93, 94]), 93);
    });
});
