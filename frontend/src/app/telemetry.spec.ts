import { describe, expect, it } from 'vitest';

import { labelFor } from './telemetry';

/** A detached element from markup. */
function markup(html: string): Element {
  const host = document.createElement('div');
  host.innerHTML = html;
  return host.firstElementChild!;
}

describe('labelFor', () => {
  it('reads the accessible name in preference to the text', () => {
    const el = markup('<button aria-label="Pause capture">pause</button>');
    expect(labelFor(el)).toBe('Pause capture');
  });

  it("strips a Material icon's ligature name out of the label", () => {
    // mat-icon's ligature is text; unstripped, this logs "micRecord".
    const el = markup('<button><mat-icon>mic</mat-icon>Record</button>');
    expect(labelFor(el)).toBe('Record');
  });

  it('ignores anything hidden from assistive technology', () => {
    const el = markup('<button><span aria-hidden="true">×</span>Dismiss</button>');
    expect(labelFor(el)).toBe('Dismiss');
  });

  it('finds the control a tap landed inside', () => {
    // A tap's target is the innermost node.
    const button = markup('<button><span class="label">Timeline</span></button>');
    expect(labelFor(button.querySelector('.label'))).toBe('Timeline');
  });

  it('says nothing for a tap that missed every control', () => {
    expect(labelFor(markup('<p>Nothing captured yet.</p>'))).toBeNull();
    expect(labelFor(null)).toBeNull();
  });

  it('does not disturb the live DOM while reading a label', () => {
    const el = markup('<button><mat-icon>search</mat-icon>Search</button>');
    document.body.append(el);
    expect(labelFor(el)).toBe('Search');
    expect(el.querySelector('mat-icon')).not.toBeNull();
    el.remove();
  });
});
