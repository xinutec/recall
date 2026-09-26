import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { provideRouter } from '@angular/router';
import { MatSnackBar } from '@angular/material/snack-bar';
import { vi } from 'vitest';

import { Session } from './session';
import { ConversationPage, Transcript } from '../models';

function turn(o: Partial<Transcript>): Transcript {
  return {
    id: 0,
    start: '2026-01-15T10:33:00Z',
    end: '2026-01-15T10:33:05Z',
    text: 't',
    language: 'en',
    speaker: null,
    speakerConfirmed: false,
    wordsChecked: false,
    speakerConfidence: null,
    confidence: null,
    loudness: null,
    model: 'diarized',
    tier: 'diarized',
    hidden: null,
    audioUrl: '/a',
    source: 'm',
    cluster: null,
    ...o,
  };
}

const said = (id: number, cluster: string, speaker?: string): Transcript =>
  turn({ id, cluster, speaker: speaker ?? null, speakerConfirmed: !!speaker });

function pageOf(turns: Transcript[]): ConversationPage {
  const start = turns[0]?.start ?? '2026-01-15T10:33:00Z';
  const end = turns.at(-1)?.end ?? start;
  return {
    items: turns.length
      ? [
          {
            start,
            end,
            turnCount: turns.length,
            speakers: [],
            preview: 'x',
            moments: [{ start, end, primary: turns, alternates: [], sources: ['m'] }],
          },
        ]
      : [],
    hasMore: false,
  };
}

async function setup(turns: Transcript[] = [], known: string[] = []) {
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      provideHttpClient(),
      provideHttpClientTesting(),
      provideRouter([]),
      { provide: MatSnackBar, useValue: { open: vi.fn() } },
    ],
  });
  const fixture = TestBed.createComponent(Session);
  fixture.componentRef.setInput('id', 'm');
  const ctrl = TestBed.inject(HttpTestingController);
  fixture.detectChanges(); // fire the three httpResources
  ctrl.match((r) => r.url.includes('/api/conversations')).forEach((r) => r.flush(pageOf(turns)));
  ctrl.match((r) => r.url.includes('/api/speakers')).forEach((r) => r.flush({ names: known }));
  await fixture.whenStable();
  fixture.detectChanges();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any -- the component's private state, reached in a test
  const c = fixture.componentInstance as any;
  return { fixture, c, ctrl };
}

describe('Session', () => {
  it('groups voices by cluster, biggest first, named only on a majority', async () => {
    const { c } = await setup([
      said(1, 'A', 'Pippijn'),
      said(2, 'A', 'Pippijn'),
      said(3, 'A', 'Pippijn'),
      said(4, 'B', 'Dr. Adams'),
      said(5, 'B'), // 1 of 2 confirmed: not a majority
    ]);
    const voices = c.voices();
    expect(voices.map((v: { cluster: string }) => v.cluster)).toEqual(['A', 'B']);
    expect(voices[0].name).toBe('Pippijn');
    expect(voices[0].turns).toBe(3);
    expect(voices[1].name).toBeNull();
  });

  it('numbers the voices for the transcript', async () => {
    const { c } = await setup([said(1, 'A'), said(2, 'B'), said(3, 'B')]);
    expect([...c.voiceNames().entries()]).toEqual([
      ['B', 'Voice 1'],
      ['A', 'Voice 2'],
    ]);
  });

  it('is finalizing while any line is provisional', async () => {
    const { c } = await setup([said(1, 'A'), turn({ id: 2, tier: 'transcribed' })]);
    expect(c.finalizing()).toBe(true);
  });

  it('naming a voice posts it for the whole session', async () => {
    const { c, ctrl } = await setup([said(1, 'A')]);
    c.nameVoice(c.voices()[0], ' Dr. Adams ');
    const req = ctrl.expectOne('/api/sessions/m/voice');
    expect(req.request.body).toEqual({ cluster: 'A', name: 'Dr. Adams' });
  });

  it('leaving a voice field unchanged posts nothing', async () => {
    const { c, ctrl } = await setup([said(1, 'A')]);
    const v = c.voices()[0];
    c.nameVoice(v, v.name ?? '');
    ctrl.expectNone('/api/sessions/m/voice');
  });

  it('a voice sample toggles, and stops when the view goes', async () => {
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue(undefined);
    vi.spyOn(HTMLMediaElement.prototype, 'pause').mockReturnValue(undefined);
    const { fixture, c } = await setup([said(1, 'A', 'Pippijn')]);
    const voice = c.voices()[0];
    c.toggleSample(voice);
    expect(c.player.playing()).toBe('voice:A');
    c.toggleSample(voice);
    expect(c.player.playing()).toBeNull();
    c.toggleSample(voice);
    fixture.destroy();
    expect(c.player.playing()).toBeNull();
  });
});
