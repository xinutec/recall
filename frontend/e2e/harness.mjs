// The app-specific half of the shared phone-width harness (@xinutec/ui-harness).
// Read by BOTH playwright.config.ts and the harness's static server, so there is
// one place to say what this app is and no port to keep in step — the port is
// allocated from `app`.

/** @type {import('@xinutec/ui-harness/config').HarnessSpec} */
export default {
  app: 'recall',
  dist: 'dist/recall-web/browser',
  // verify.sh points the harness at its scratch build (RECALL_E2E_DIST) so the
  // run reuses that one build instead of producing a second.
  distEnv: 'RECALL_E2E_DIST',
  // No API stub: the specs page.route everything, and anything they leave
  // unrouted answers `[]`, which is enough to stay in the app shell.
};
