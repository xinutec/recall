import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { ActivatedRoute, Router, convertToParamMap } from '@angular/router';
import { Location } from '@angular/common';
import { MatSnackBar } from '@angular/material/snack-bar';
import { BehaviorSubject, Subject, of } from 'rxjs';
import { vi } from 'vitest';

import { Train } from './train';
import { RecallApi } from '../recall-api';
import { Transcript } from '../models';

const ITEM: Transcript = {
  id: 1,
  start: '2026-06-14T17:00:00Z',
  end: '2026-06-14T17:00:02Z',
  text: 'hallo',
  language: 'nl',
  speaker: null,
  speakerConfirmed: false,
  speakerConfidence: null,
  confidence: 0.5,
  loudness: 0.03,
  model: 'whisper',
  tier: 'transcribed',
  hidden: null,
  audioUrl: '/api/audio/1',
  source: null,
  cluster: null,
};

function setup(params: Record<string, string> = {}, items: Transcript[] = [ITEM]) {
  const navigate = vi.fn().mockResolvedValue(true);
  const trainQueue = vi
    .fn()
    .mockReturnValue(of({ items, corrections: 3, bySpeaker: { Alice: 2, Carol: 1 } }));
  const correct = vi.fn().mockReturnValue(of({ newId: 9 }));
  const around = vi.fn().mockReturnValue(of({ before: [], after: [] }));
  const split = vi.fn().mockReturnValue(of({ newIds: [9, 10] }));
  const unintelligible = vi.fn().mockReturnValue(of({ ok: true }));
  const suggest = vi.fn().mockReturnValue(of({ speaker: null }));
  const transcripts = vi.fn().mockReturnValue(of({ items: [ITEM] }));
  const speakers = vi.fn().mockReturnValue(of({ names: ['Alice', 'Bob', 'Carol', 'Pippijn'] }));
  const back = vi.fn();

  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      {
        provide: RecallApi,
        useValue: {
          trainQueue,
          correct,
          around,
          split,
          unintelligible,
          suggest,
          transcripts,
          speakers,
        },
      },
      { provide: Router, useValue: { navigate } },
      {
        provide: ActivatedRoute,
        useValue: { queryParamMap: new BehaviorSubject(convertToParamMap(params)) },
      },
      { provide: MatSnackBar, useValue: { open: vi.fn() } },
      { provide: Location, useValue: { back } },
    ],
  });
  const fixture = TestBed.createComponent(Train);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const c = fixture.componentInstance as any;
  return {
    fixture,
    c,
    navigate,
    trainQueue,
    correct,
    split,
    unintelligible,
    suggest,
    transcripts,
    back,
  };
}

describe('Train', () => {
  it('applyRange writes the picked window to the URL', () => {
    const { c, navigate } = setup();
    c.from.set('2026-06-14T18:00');
    c.to.set('2026-06-14T18:50');
    c.applyRange();
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({
        queryParams: { from: '2026-06-14T18:00', to: '2026-06-14T18:50' },
      }),
    );
  });

  it('clearRange removes the window from the URL', () => {
    const { c, navigate } = setup({ from: '2026-06-14T18:00' });
    c.clearRange();
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { from: null, to: null } }),
    );
  });

  it('loads the queue scoped to the URL range', () => {
    const { fixture, trainQueue } = setup({ from: '2026-06-14T18:00', to: '2026-06-14T18:50' });
    fixture.detectChanges(); // flush effects
    expect(trainQueue).toHaveBeenCalled();
    const call = trainQueue.mock.calls.at(-1) as [number, string?, string?];
    expect(call[0]).toBe(40);
    expect(call[1]).toBeTruthy(); // since present
    expect(call[2]).toBeTruthy(); // until present
  });

  it('toggling to chronological writes order=time to the URL', () => {
    const { c, navigate } = setup();
    c.setOrder('time');
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { order: 'time' } }),
    );
  });

  it('toggling back to loudest drops order from the URL (the default)', () => {
    const { c, navigate } = setup({ order: 'time' });
    c.setOrder('loudness');
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { order: null } }),
    );
  });

  it('loads with the order taken from the URL', () => {
    const { fixture, trainQueue } = setup({ order: 'time' });
    fixture.detectChanges();
    const call = trainQueue.mock.calls.at(-1) as [number, string?, string?, string?];
    expect(call[3]).toBe('time');
  });

  it('editing a time field does not reload until Apply', () => {
    const { fixture, c, trainQueue } = setup({ from: '2026-06-14T18:00' });
    fixture.detectChanges(); // initial load from the URL window
    const before = trainQueue.mock.calls.length;
    // User changes the field (the signal updates) but has NOT pressed Apply.
    c.from.set('2026-06-14T19:00');
    fixture.detectChanges();
    expect(trainQueue.mock.calls.length).toBe(before); // the batch only changes on Apply
  });

  it('save needs a speaker tag — untagged fragments are not trainable', () => {
    const { c, correct } = setup();
    c.queue.set([ITEM]);
    c.index.set(0);
    c.draft.set('hallo daar');
    // no speaker picked
    c.save();
    expect(correct).not.toHaveBeenCalled();
  });

  it('pre-selects the suggested speaker for the current turn (one-tap confirm)', () => {
    const { fixture, c, suggest } = setup();
    suggest.mockReturnValue(of({ speaker: 'Alice' }));
    c.queue.set([ITEM]);
    c.index.set(0);
    fixture.detectChanges(); // flush the per-turn effect (around + suggest)
    expect(suggest).toHaveBeenCalledWith(1);
    expect(c.suggested()).toBe('Alice');
    expect(c.speaker()).toBe('Alice'); // pre-selected so Save is one tap
  });

  it('pre-selects an already-confirmed turn from its existing label', () => {
    const { fixture, c, suggest, trainQueue } = setup();
    suggest.mockReturnValue(of({ speaker: null })); // faint clip → no voiceprint guess
    trainQueue.mockReturnValue(
      of({
        items: [{ ...ITEM, speaker: 'Pippijn', speakerConfirmed: true }],
        corrections: 0,
        bySpeaker: {},
      }),
    );
    fixture.detectChanges();
    expect(c.speaker()).toBe('Pippijn'); // from the label, even with no suggestion
  });

  it('a suggestion does not override a speaker the user already picked', () => {
    const { fixture, c, suggest } = setup();
    const pending = new Subject<{ speaker: string | null }>();
    suggest.mockReturnValue(pending); // suggestion is slow (model load)
    c.queue.set([ITEM]);
    c.index.set(0);
    fixture.detectChanges(); // turn loads, suggestion in flight
    c.speaker.set('Carol'); // user picks before it arrives
    pending.next({ speaker: 'Alice' }); // suggestion lands late
    expect(c.speaker()).toBe('Carol'); // user's choice respected
  });

  it('exposes per-speaker counts so the corpus can be balanced', () => {
    const { fixture, c } = setup();
    fixture.detectChanges();
    expect(c.bySpeaker()).toEqual({ Alice: 2, Carol: 1 });
    expect(c.speakerCount('Alice')).toBe(2);
    expect(c.speakerCount('Pippijn')).toBe(0);
  });

  it('save bumps the per-voice count immediately (no reload needed)', () => {
    const { c } = setup();
    c.queue.set([ITEM, { ...ITEM, id: 2 }]); // 2 items so advance steps, not reload
    c.index.set(0);
    c.bySpeaker.set({ Carol: 1 });
    c.draft.set('hoi');
    c.speaker.set('Carol');
    c.save();
    expect(c.speakerCount('Carol')).toBe(2);
  });

  it('saveSplit bumps each fragment voice immediately', () => {
    const { c } = setup();
    c.queue.set([ITEM, { ...ITEM, id: 2 }]);
    c.index.set(0);
    c.bySpeaker.set({});
    c.splitting.set(true);
    c.parts.set([
      { start: 'a', end: 'b', text: 'one', speaker: 'Carol' },
      { start: 'b', end: 'c', text: 'two', speaker: 'Alice' },
    ]);
    c.saveSplit();
    expect(c.speakerCount('Carol')).toBe(1);
    expect(c.speakerCount('Alice')).toBe(1);
  });

  it('rates clip clarity from loudness so faint audio can be skipped', () => {
    const { c } = setup();
    expect(c.clarity({ ...ITEM, loudness: 0.5 })).toBe('clear');
    expect(c.clarity({ ...ITEM, loudness: 0.02 })).toBe('quiet');
    expect(c.clarity({ ...ITEM, loudness: 0.001 })).toBe('faint');
    expect(c.clarity({ ...ITEM, loudness: null })).toBe('faint');
  });

  it('the current turn seeds the language; save sends the corrected one', () => {
    const { fixture, c, correct, trainQueue } = setup();
    // Machine heard English; the queue must come through load() (detectChanges
    // refetches), so seed it via the queue mock.
    trainQueue.mockReturnValue(
      of({ items: [{ ...ITEM, language: 'en' }], corrections: 3, bySpeaker: {} }),
    );
    fixture.detectChanges(); // per-turn effect seeds language from the turn
    expect(c.language()).toBe('en');
    c.setLanguage('nl'); // user corrects: it was actually Dutch
    c.draft.set('Nee? Deze niet?');
    c.speaker.set('Alice');
    c.save();
    expect(correct).toHaveBeenCalledWith(
      1,
      'Nee? Deze niet?',
      expect.objectContaining({ language: 'nl' }),
    );
  });

  it('save posts the corrected text and tagged speaker', () => {
    const { c, correct } = setup();
    c.queue.set([ITEM]);
    c.index.set(0);
    c.draft.set('hallo daar');
    c.speaker.set('Alice');
    c.save();
    expect(correct).toHaveBeenCalledWith(
      1,
      'hallo daar',
      expect.objectContaining({ speaker: 'Alice' }),
    );
  });

  it('saveSplit posts every per-speaker fragment', () => {
    const { c, split } = setup();
    c.queue.set([ITEM]);
    c.index.set(0);
    c.splitting.set(true);
    c.parts.set([
      { start: 'a', end: 'b', text: 'one', speaker: 'Carol' },
      { start: 'b', end: 'c', text: 'two', speaker: 'Alice' },
    ]);
    c.saveSplit();
    expect(split).toHaveBeenCalledWith(
      1,
      expect.arrayContaining([
        expect.objectContaining({ text: 'one', speaker: 'Carol' }),
        expect.objectContaining({ text: 'two', speaker: 'Alice' }),
      ]),
    );
  });

  it('Back steps to the previous clip and stops at the first', () => {
    const { c } = setup();
    c.queue.set([ITEM, { ...ITEM, id: 2 }, { ...ITEM, id: 3 }]);
    c.index.set(2);
    c.back();
    expect(c.index()).toBe(1);
    c.back();
    expect(c.index()).toBe(0);
    c.back();
    expect(c.index()).toBe(0); // clamped at the start
  });

  it('the B key steps back', () => {
    const { c } = setup();
    c.queue.set([ITEM, { ...ITEM, id: 2 }]);
    c.index.set(1);
    c.onKey({ key: 'b', preventDefault: () => undefined, target: document.body });
    expect(c.index()).toBe(0);
  });

  it('the number keys pick the matching speaker from the roster', () => {
    const { c } = setup();
    c.queue.set([ITEM]);
    c.index.set(0);
    // Roster order is whatever /api/speakers returns: 1=Alice 2=Bob 3=Carol 4=Pippijn.
    c.onKey({ key: '2', preventDefault: () => undefined, target: document.body });
    expect(c.speaker()).toBe('Bob');
    c.onKey({ key: '4', preventDefault: () => undefined, target: document.body });
    expect(c.speaker()).toBe('Pippijn');
  });

  // The real-device failure: the native datetime-local picker fires NEITHER
  // input nor change on this phone, so the signal stays empty while the field
  // visibly holds a value — and Apply navigated with from=null. Apply must read
  // the field's value directly (pull on submit), not depend on an event having
  // pushed it into the signal.
  it('Apply reads the field value even when no input/change event fired', () => {
    const { fixture, c, navigate } = setup();
    fixture.detectChanges();
    const from = fixture.nativeElement.querySelector(
      'input[type="datetime-local"]',
    ) as HTMLInputElement;
    from.value = '2026-06-14T18:00'; // value present, but NO event dispatched
    expect(c.from()).toBe(''); // signal never received it (like the phone)
    c.applyRange();
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({
        queryParams: expect.objectContaining({ from: '2026-06-14T18:00' }),
      }),
    );
  });

  // Regression for the bug that started all this: on mobile the datetime-local
  // picker emits `change`, not `input`, so a (input)-only binding left the
  // signal — and therefore Apply — stuck on the empty value. Drive the real DOM
  // event so a future template edit can't silently reintroduce it.
  it('a datetime-local change updates the window and Apply sends it', () => {
    const { fixture, c, navigate } = setup();
    fixture.detectChanges();
    const from = fixture.nativeElement.querySelector(
      'input[type="datetime-local"]',
    ) as HTMLInputElement;
    from.value = '2026-06-14T18:00';
    from.dispatchEvent(new Event('change'));
    expect(c.from()).toBe('2026-06-14T18:00');
    c.applyRange();
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { from: '2026-06-14T18:00', to: null } }),
    );
  });

  it('with ?id loads just that turn for labelling (timeline → label this)', () => {
    const { fixture, c, transcripts } = setup({ id: '1' });
    fixture.detectChanges(); // flush effects

    expect(c.targeted()).toBe(true);
    expect(transcripts).toHaveBeenCalledWith('1');
    expect(c.current()).toEqual(ITEM);
  });

  it('saving a targeted turn returns to where you came from', () => {
    const { fixture, c, correct, back } = setup({ id: '1' });
    fixture.detectChanges();

    c.speaker.set('Carol'); // a speaker is required to save
    c.save();

    expect(correct).toHaveBeenCalled();
    expect(back).toHaveBeenCalled();
  });
});

describe('Train — stale in-flight responses', () => {
  const ITEM2: Transcript = { ...ITEM, id: 2, text: 'tweede' };

  it('a late suggestion from the previous turn never pre-selects a speaker', () => {
    const slow = new Subject<{ speaker: string | null; confidence?: number }>();
    const { fixture, c, suggest } = setup({}, [ITEM, ITEM2]);
    suggest.mockImplementation((id: number) => (id === 1 ? slow : of({ speaker: null })));
    fixture.detectChanges(); // per-turn effect: suggest(1) issued, still pending

    c.advance(); // user moved on to turn 2
    fixture.detectChanges();
    slow.next({ speaker: 'Alice' }); // turn 1's reply lands late

    // Turn 2 must not inherit turn 1's voiceprint suggestion — one Enter press
    // would save it into the fine-tuning corpus.
    expect(c.suggested()).toBeNull();
    expect(c.speaker()).toBeNull();
  });

  it('late context from the previous turn never renders around the current one', () => {
    const slow = new Subject<{ before: Transcript[]; after: Transcript[] }>();
    const { fixture, c, suggest } = setup({}, [ITEM, ITEM2]);
    void suggest;
    const api = TestBed.inject(RecallApi) as unknown as { around: ReturnType<typeof vi.fn> };
    api.around.mockImplementation((id: number) =>
      id === 1 ? slow : of({ before: [], after: [] }),
    );
    fixture.detectChanges();

    c.advance();
    fixture.detectChanges();
    slow.next({ before: [ITEM], after: [ITEM] });

    expect(c.before()).toEqual([]);
    expect(c.after()).toEqual([]);
  });

  it('refilling an exhausted queue reads the committed URL window, not half-edited fields', () => {
    const { fixture, c, trainQueue } = setup({ from: '2026-06-14T18:00' });
    fixture.detectChanges();
    const initialArgs = trainQueue.mock.calls[0];
    trainQueue.mockClear();

    c.from.set('2026-06-'); // half-typed edit, never Applied
    c.advance(); // past the end of the one-item queue -> refill

    expect(trainQueue).toHaveBeenCalledTimes(1);
    // The refill must repeat the committed (URL) window, not the half-typed edit.
    expect(trainQueue.mock.calls[0]).toEqual(initialArgs);
  });
});
