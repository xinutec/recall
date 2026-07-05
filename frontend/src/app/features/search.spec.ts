import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { ActivatedRoute, Router } from '@angular/router';
import { vi } from 'vitest';

import { Search } from './search';

function setup() {
  const navigate = vi.fn();
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: Router, useValue: { navigate } },
      { provide: ActivatedRoute, useValue: {} },
    ],
  });
  const fixture = TestBed.createComponent(Search);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const c = fixture.componentInstance as any;
  return { fixture, c, navigate };
}

describe('Search', () => {
  it('mirrors typing into the URL as ?q without spamming history', () => {
    const { c, navigate } = setup();
    c.onInput('verzameling');
    expect(c.query()).toBe('verzameling');
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { q: 'verzameling' }, replaceUrl: true }),
    );
  });

  it('clearing the box drops ?q from the URL', () => {
    const { c, navigate } = setup();
    c.onInput('');
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { q: null }, replaceUrl: true }),
    );
  });

  it('seeds the query from the ?q input so a shared link loads pre-filled', () => {
    const { fixture, c } = setup();
    fixture.componentRef.setInput('q', 'iets');
    fixture.detectChanges(); // run the effect that copies qParam -> query
    expect(c.query()).toBe('iets');
  });

  it('does not search until at least two characters are typed', () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    try {
      const { fixture, c } = setup();
      fixture.componentRef.setInput('q', 'a');
      fixture.detectChanges(); // flush the mirror + debounce effects
      vi.advanceTimersByTime(300); // past the debounce
      expect(c.trimmedLength()).toBe(1);
      // results stays idle below the 2-char floor (httpResource request is undefined)
      expect(c.items()).toEqual([]);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('Search — debounce', () => {
  it('keystrokes only reach the FTS query after the debounce window', () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    try {
      const { fixture, c } = setup();
      fixture.componentRef.setInput('q', 'hello');
      fixture.detectChanges(); // flush the mirror + debounce effects
      // Mid-typing: the trimmed query the resource keys on must not update yet —
      // otherwise every keystroke fires a 200-row FTS request.
      expect(c.trimmed()).toBe('');
      vi.advanceTimersByTime(300);
      expect(c.trimmed()).toBe('hello');
    } finally {
      vi.useRealTimers();
    }
  });
});
