// @ts-check
// Type-aware typescript-eslint (floating promises, unsafe `any`, await-thenable)
// plus the Angular rules: no inline templates or styles, template a11y.

import angular from "angular-eslint";
import tseslint from "typescript-eslint";

export default tseslint.config(
  // An eslint-disable that disables nothing is dead debt; ESLint only warns by default.
  { linterOptions: { reportUnusedDisableDirectives: 'error' } },
  // ts-rs writes src/app/generated/ from the Rust types; generated code is not linted.
  { ignores: ["src/app/generated/**"] },
  {
    files: ["src/**/*.ts"],
    extends: [
      ...tseslint.configs.recommendedTypeChecked,
      ...tseslint.configs.stylisticTypeChecked,
      ...angular.configs.tsRecommended,
    ],
    languageOptions: {
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
    processor: angular.processInlineTemplates,
    rules: {
      "@angular-eslint/component-max-inline-declarations": ["error", { template: 0, styles: 0 }],
      // `x as Shape` is a claim, not a check, and the one way past dev-lint's
      // DL-ANGULAR-STRINGIFIED-OBJECT. Narrow at the boundary instead.
      "@typescript-eslint/no-unsafe-type-assertion": "error",
      "@typescript-eslint/no-empty-function": "off",
    },
  },
  {
    // Tests use `any` for mocks, DOM and fixtures.
    files: ["src/**/*.spec.ts"],
    rules: {
      // A test double is asserted into the interface it stands in for.
      "@typescript-eslint/no-unsafe-type-assertion": "off",
      "@typescript-eslint/no-unsafe-member-access": "off",
      "@typescript-eslint/no-unsafe-call": "off",
      "@typescript-eslint/no-unsafe-assignment": "off",
      "@typescript-eslint/no-unsafe-argument": "off",
      "@typescript-eslint/no-unsafe-return": "off",
    },
  },
  {
    // The layout harness and its specs. Type-aware for no-floating-promises: an
    // unawaited `route.fulfill(...)` still mocks the request, so the test passes.
    //
    // `project`, not `projectService`: the service binds a file to the nearest
    // tsconfig.json, which is solution-style (`"files": []`) and covers nothing.
    files: ["e2e/**/*.ts", "playwright.config.ts"],
    extends: [...tseslint.configs.recommendedTypeChecked, ...tseslint.configs.stylisticTypeChecked],
    languageOptions: {
      parserOptions: { project: ["tsconfig.e2e.json"], tsconfigRootDir: import.meta.dirname },
    },
  },
  {
    files: ["src/**/*.html"],
    extends: [...angular.configs.templateRecommended, ...angular.configs.templateAccessibility],
  },
);
