// @ts-check
// ESLint flat config for the Angular frontend. Type-aware: typescript-eslint
// recommendedTypeChecked + stylisticTypeChecked (parserOptions.projectService)
// for usage bugs tsc/syntactic-lint miss (floating/misused promises, unsafe
// `any`, await-thenable), plus the Angular rules (forbid inline template:/styles:
// — the team's angular-external-template-style rule — and template a11y).
// It's fast so it runs as the normal lint in CI; `npm run lint`.

import angular from "angular-eslint";
import tseslint from "typescript-eslint";

export default tseslint.config(
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
      // `x as Shape` is a claim, not a check — and it is the one hole in the
      // otherwise-total protection against a value reaching the screen in the
      // wrong shape. dev-lint's DL-ANGULAR-STRINGIFIED-OBJECT types every
      // template expression honestly, so the only way to fool it is with a type
      // we manufactured ourselves. Narrow at the boundary instead.
      "@typescript-eslint/no-unsafe-type-assertion": "error",
      "@typescript-eslint/no-empty-function": "off",
    },
  },
  {
    // Tests legitimately use `any` for mocks / DOM / fixtures — relax the
    // unsafe-any family here; app code stays fully type-checked.
    files: ["src/**/*.spec.ts"],
    rules: {
      // A double asserted into the interface it stands in for is the whole
      // point of a double; getting it wrong fails a test, it never reaches a
      // user. App code stays strict.
      "@typescript-eslint/no-unsafe-type-assertion": "off",
      "@typescript-eslint/no-unsafe-member-access": "off",
      "@typescript-eslint/no-unsafe-call": "off",
      "@typescript-eslint/no-unsafe-assignment": "off",
      "@typescript-eslint/no-unsafe-argument": "off",
      "@typescript-eslint/no-unsafe-return": "off",
    },
  },
  {
    // The layout harness and its specs. The blocks above say `src`, so until
    // this existed the e2e tree was linted by nothing, on top of being
    // type-checked by nothing (see tsconfig.e2e.json). It is the only gate that
    // can see what a phone actually suffers, which makes "nobody checks it" the
    // wrong property for it to have.
    //
    // Type-aware, and that is the point: the rule that pays here is
    // no-floating-promises. A `route.fulfill(...)` dropped inside a route
    // handler still mocks the request, so the test passes and nothing says the
    // handler returned before the fulfilment finished.
    //
    // ⚠ `project`, not `projectService`, and only here. The service discovers a
    // file's project by walking up to the nearest tsconfig.json — and this
    // repo's is solution-style, `"files": []`, so it claims nothing and the
    // specs bind to no project at all. The symptom is a parsing error per file,
    // which at least fails loudly; the danger is "fixing" it by loosening the
    // rules until the noise stops. Naming tsconfig.e2e.json is the honest
    // answer: it is the config that actually covers these files.
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
