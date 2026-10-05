// The types for `harness.mjs`, which cannot be TypeScript.
//
// It is `.mjs` because the harness's static server loads it under plain Node,
// as well as the compiled Playwright config. Without this it is an implicit `any`.
import type { HarnessSpec } from '@xinutec/ui-harness/config';

declare const spec: HarnessSpec;
export default spec;
