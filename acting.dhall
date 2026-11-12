{-
recall/acting.dhall — every operational script in this repository (`*.sh`,
`*.py`), classified, with a ceiling per class; a test needs no row when its path
says it is one. What the classes mean and when a ceiling may rise:
dev-lint/tables/acting_schema.dhall. The gate runs `dev-lint-fleet --acting .`:
a script with no row, a row for a file that is gone, or a class over its
ceiling fails the commit that causes it.
-}
let A = ../dev-lint/tables/acting_schema.dhall

let subject = A.subject

let Class = A.Class

in  { ceilings =
        -- acts: each row names what it becomes (automation-model items 13-15)
      { acts = 1
      , bootstrap = 0
      , by_design = 0
      , observes = 0
      , observer_checks = 0
      }
    , subjects =
      [ subject
              "android/deploy.sh"
              Class.Acts
              "builds the APK and installs it on a phone over adb: the phone-shells plan's target (automation-model item 14)"
          , subject "frontend/gen-icons.sh" Class.OutOfScope ""
          , subject "scripts/check-doc-paths.sh" Class.OutOfScope ""
          , subject "scripts/check-pii.sh" Class.OutOfScope ""
          , subject "scripts/check_pure_crate.py" Class.OutOfScope ""
          , subject "scripts/check_workspace_members.py" Class.OutOfScope ""
          , subject "scripts/diarize_sweep.py" Class.OutOfScope ""
          , subject "scripts/gen-speech-fixture.sh" Class.OutOfScope ""
          , subject "scripts/gen-types.sh" Class.OutOfScope ""
          , subject "scripts/gen_latin_ranges.py" Class.OutOfScope ""
          , subject "scripts/ios-test.sh" Class.OutOfScope ""
          , subject "scripts/recall-build-frontend.sh" Class.OutOfScope ""
          , subject "scripts/sandbox_sampler.sh" Class.OutOfScope ""
          , subject "scripts/setup-hooks.sh" Class.OutOfScope ""
          , subject
              "scripts/stall_base_rate.py"
              Class.OutOfScope
              "names restic and rsync as processes to count; runs neither"
          , subject "src/recall/__init__.py" Class.OutOfScope ""
          , subject "src/recall/asr.py" Class.OutOfScope ""
          , subject "src/recall/diarize.py" Class.OutOfScope ""
          , subject "src/recall/score_asr.py" Class.OutOfScope ""
          , subject "src/recall/shim.py" Class.OutOfScope ""
          , subject "src/recall/shim_asr.py" Class.OutOfScope ""
          , subject "src/recall/shim_voices.py" Class.OutOfScope ""
          , subject "src/recall/speakerid.py" Class.OutOfScope ""
          , subject "src/recall/wer.py" Class.OutOfScope ""
      ]
    }
