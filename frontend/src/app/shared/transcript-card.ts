import { ChangeDetectionStrategy, Component, input } from '@angular/core';
import { MatCardModule } from '@angular/material/card';
import { MatChipsModule } from '@angular/material/chips';
import { MatIconModule } from '@angular/material/icon';

import { Transcript } from '../models';
import { formatClock, formatConfidence, formatDuration } from '../format';

/** Read-only display of a single transcript turn with inline audio playback. */
@Component({
  selector: 'app-transcript-card',
  imports: [MatCardModule, MatChipsModule, MatIconModule],
  templateUrl: './transcript-card.html',
  styleUrl: './transcript-card.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class TranscriptCard {
  readonly transcript = input.required<Transcript>();

  protected readonly clock = (t: Transcript): string => formatClock(t.start);
  protected readonly duration = formatDuration;
  protected readonly confidence = (t: Transcript): string => formatConfidence(t.confidence);
}
