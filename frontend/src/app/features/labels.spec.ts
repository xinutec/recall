import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { ActivatedRoute, Router } from '@angular/router';
import { MatSnackBar } from '@angular/material/snack-bar';
import { of, throwError } from 'rxjs';
import { vi } from 'vitest';

import { Labels } from './labels';
import { RecallApi } from '../recall-api';

function setup(speaker = '') {
  const navigate = vi.fn();
  const open = vi.fn();
  const reassignCorrection = vi.fn(() => of({ ok: true }));
  const hideCorrection = vi.fn(() => of({ ok: true }));
  const addVocabularyTerm = vi.fn(() => of({ newId: 5 }));
  const deleteVocabularyTerm = vi.fn(() => of({ ok: true }));
  const setContext = vi.fn((text: string) => {
    void text;
    return of({ ok: true });
  });
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: Router, useValue: { navigate } },
      { provide: ActivatedRoute, useValue: {} },
      {
        provide: RecallApi,
        useValue: {
          reassignCorrection,
          hideCorrection,
          addVocabularyTerm,
          deleteVocabularyTerm,
          setContext,
        },
      },
      { provide: MatSnackBar, useValue: { open } },
    ],
  });
  const fixture = TestBed.createComponent(Labels);
  fixture.componentRef.setInput('speaker', speaker);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const c = fixture.componentInstance as any;
  return {
    fixture,
    c,
    navigate,
    open,
    reassignCorrection,
    hideCorrection,
    addVocabularyTerm,
    deleteVocabularyTerm,
    setContext,
  };
}

describe('Labels', () => {
  it('re-tag calls the API with the new voice', () => {
    const { c, reassignCorrection } = setup('Carol');
    c.reassign(5, 'Alice');
    expect(reassignCorrection).toHaveBeenCalledWith(5, 'Alice');
  });

  it('remove hides the label', () => {
    const { c, hideCorrection } = setup();
    c.remove(7);
    expect(hideCorrection).toHaveBeenCalledWith(7);
  });

  it('picking a voice puts it in the URL', () => {
    const { c, navigate } = setup();
    c.pick('Bob');
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { speaker: 'Bob' } }),
    );
  });

  it('plays the exact cut by default, the padded clip with context on', () => {
    const { c } = setup();
    const label = { audioUrl: '/api/correction/5/audio' };
    expect(c.audioSrc(label)).toBe('/api/correction/5/audio'); // exact, no param
    c.setContext(true);
    expect(c.audioSrc(label)).toBe('/api/correction/5/audio?context=true');
  });

  it('saving the household context sends the trimmed draft to the API', () => {
    const { c, setContext } = setup();
    c.contextDraft.set('  Rufus is the family dog.  ');
    expect(c.contextDirty()).toBe(true); // differs from the (unloaded) stored value

    c.saveContext();
    expect(setContext).toHaveBeenCalledWith('Rufus is the family dog.');
    expect(c.contextDraft()).toBeNull(); // editor re-synced to the stored value
  });

  it('nudging a boundary calls the API and re-fetches the clip', () => {
    const nudgeCorrection = vi.fn(() => of({ ok: true }));
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        provideHttpClient(),
        provideHttpClientTesting(),
        { provide: Router, useValue: { navigate: vi.fn() } },
        { provide: ActivatedRoute, useValue: {} },
        { provide: RecallApi, useValue: { nudgeCorrection } },
        { provide: MatSnackBar, useValue: { open: vi.fn() } },
      ],
    });
    const fixture = TestBed.createComponent(Labels);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const c = fixture.componentInstance as any;
    const label = { audioUrl: '/api/correction/9/audio' };
    c.nudge(9, 'start', -0.1); // widen: start earlier
    expect(nudgeCorrection).toHaveBeenCalledWith(9, 'start', -0.1);
    expect(c.audioSrc(label)).toBe('/api/correction/9/audio?v=1'); // cache-bust → re-fetch
  });

  it('reports a failed re-tag via the snackbar (not silently)', () => {
    const open = vi.fn();
    const reassignCorrection = vi.fn(() => throwError(() => new Error('nope')));
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        provideHttpClient(),
        provideHttpClientTesting(),
        { provide: Router, useValue: { navigate: vi.fn() } },
        { provide: ActivatedRoute, useValue: {} },
        { provide: RecallApi, useValue: { reassignCorrection } },
        { provide: MatSnackBar, useValue: { open } },
      ],
    });
    const fixture = TestBed.createComponent(Labels);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const c = fixture.componentInstance as any;
    c.reassign(5, 'Alice');
    expect(open).toHaveBeenCalled();
  });
});

describe('Labels — vocabulary', () => {
  it('adds a trimmed term and clears the input', () => {
    const { c, addVocabularyTerm } = setup();
    c.newTerm.set('  EGA wing ');
    c.addTerm();
    expect(addVocabularyTerm).toHaveBeenCalledWith('EGA wing');
    expect(c.newTerm()).toBe('');
  });

  it('never posts a blank term', () => {
    const { c, addVocabularyTerm } = setup();
    c.newTerm.set('   ');
    c.addTerm();
    expect(addVocabularyTerm).not.toHaveBeenCalled();
  });

  it('removes a term by id', () => {
    const { c, deleteVocabularyTerm } = setup();
    c.removeTerm(7);
    expect(deleteVocabularyTerm).toHaveBeenCalledWith(7);
  });
});
