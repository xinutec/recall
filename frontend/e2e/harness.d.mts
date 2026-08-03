// The types for `harness.mjs`, which cannot be TypeScript.
//
// ⚠ It is `.mjs` because two things with different needs import it: the
// Playwright config, which is compiled, and the harness's own static server,
// which runs it under plain Node. A `.ts` would satisfy the first and be
// unloadable by the second, so the file stays JavaScript and its type lives
// here. Without this it is an implicit `any`, and the config that hands it to
// `phoneConfig` is checked against nothing.
import type { HarnessSpec } from '@xinutec/ui-harness/config';

declare const spec: HarnessSpec;
export default spec;
