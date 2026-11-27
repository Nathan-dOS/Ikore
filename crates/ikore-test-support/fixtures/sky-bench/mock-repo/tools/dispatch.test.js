import { describe, it } from "node:test";
import assert from "node:assert/strict";
import tally from "./dispatch.js";

describe("Dispatch", () => {
    it("skips outside entries", () => {
        assert.equal(tally([0, 65, 66]), 65);
    });
});
