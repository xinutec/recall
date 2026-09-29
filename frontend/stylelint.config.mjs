// stylelint's own correctness checks, at full strength (dev-lint's standards §7).
// Colours are dev-lint's DL-SCSS-HARDCODED-COLOR, with its waivers, not this file's.
export default {
  extends: ['stylelint-config-recommended-scss'],
  rules: {
    // A bare `//` is the paragraph break inside a comment block.
    'scss/comment-no-empty': null,
  },
};
