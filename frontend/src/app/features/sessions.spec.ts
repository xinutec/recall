import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { Router } from '@angular/router';
import { MatSnackBar } from '@angular/material/snack-bar';
import { of, throwError } from 'rxjs';
import { vi } from 'vitest';

import { Sessions } from './sessions';
import { RecallApi } from '../recall-api';

function setup(createOk = true) {
  const navigate = vi.fn();
  const open = vi.fn();
  const createSession = vi.fn((...args: [File, string, string]) => {
    void args;
    return createOk
      ? of({ id: 'meeting-20260703-1420', title: 'Meeting 2026-07-03 14:20' })
      : throwError(() => new Error('network'));
  });
  const renameSession = vi.fn(() => of({ ok: true }));
  const deleteSession = vi.fn(() => of({ ok: true }));
  const rediarizeSession = vi.fn(() => of({ ok: true }));
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: Router, useValue: { navigate } },
      {
        provide: RecallApi,
        useValue: { createSession, renameSession, deleteSession, rediarizeSession },
      },
      { provide: MatSnackBar, useValue: { open } },
    ],
  });
  const fixture = TestBed.createComponent(Sessions);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const c = fixture.componentInstance as any;
  return { fixture, c, navigate, open, createSession, renameSession, deleteSession, rediarizeSession };
}

function fileInput(file: File | null): HTMLInputElement {
  return {
    files: file ? [file] : [],
    value: 'x',
  } as unknown as HTMLInputElement;
}

describe('Sessions', () => {
  it('uploads a picked file with its last-modified time as the start', () => {
    const { c, createSession } = setup();
    const file = new File(['audio'], 'appointment.mp3', {
      type: 'audio/mpeg',
      lastModified: Date.UTC(2026, 6, 3, 13, 20, 0), // 2026-07-03 14:20 BST
    });

    c.onFile(fileInput(file));

    expect(createSession).toHaveBeenCalledTimes(1);
    const [sent, title, start] = createSession.mock.calls[0];
    expect(sent).toBe(file);
    expect(title).toBe(''); // default title comes from the backend
    expect(start).toBe(new Date(file.lastModified).toISOString());
    expect(c.uploading()).toBe(false); // cleared on success
  });

  it('ignores an empty pick and clears the failed-upload flag', () => {
    const { c, createSession, open } = setup(false);
    c.onFile(fileInput(null));
    expect(createSession).not.toHaveBeenCalled();

    const file = new File(['a'], 'x.mp3', { type: 'audio/mpeg' });
    c.onFile(fileInput(file));
    expect(c.uploading()).toBe(false); // error path resets the spinner
    expect(open).toHaveBeenCalled(); // and reports the failure
  });

  it('rename only fires with a non-empty title, then closes the editor', () => {
    const { c, renameSession } = setup();
    c.startEdit({ id: 'meeting-1', title: 'old' });
    expect(c.editingId()).toBe('meeting-1');

    c.editTitle.set('   ');
    c.saveEdit('meeting-1');
    expect(renameSession).not.toHaveBeenCalled(); // blank rejected

    c.editTitle.set('Neuro clinic');
    c.saveEdit('meeting-1');
    expect(renameSession).toHaveBeenCalledWith('meeting-1', 'Neuro clinic');
    expect(c.editingId()).toBeNull();
  });

  it('delete is two-step: confirm required before the API is hit', () => {
    const { c, deleteSession } = setup();
    c.askDelete('meeting-1');
    expect(c.confirmingId()).toBe('meeting-1');
    expect(deleteSession).not.toHaveBeenCalled(); // not on the first tap

    c.confirmDelete('meeting-1');
    expect(deleteSession).toHaveBeenCalledWith('meeting-1');
    expect(c.confirmingId()).toBeNull();
  });

  it('re-diarize queues without navigating away', () => {
    const { c, rediarizeSession, navigate } = setup();
    c.rediarize('meeting-1');
    expect(rediarizeSession).toHaveBeenCalledWith('meeting-1');
    expect(navigate).not.toHaveBeenCalled();
  });
});
