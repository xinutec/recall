import { ChangeDetectionStrategy, Component, input } from '@angular/core';
import { RouterLink } from '@angular/router';
import { MatCardModule } from '@angular/material/card';
import { MatChipsModule } from '@angular/material/chips';
import { MatIconModule } from '@angular/material/icon';

import { Transcript } from '../models';
import { formatClock } from '../format';
import { PlayButton } from './play-button';

/** One line, read-only: who, when, its words and its audio. Its time opens
 * the conversation it was said in. */
@Component({
  selector: 'app-transcript-card',
  imports: [PlayButton, RouterLink, MatCardModule, MatChipsModule, MatIconModule],
  templateUrl: './transcript-card.html',
  styleUrl: './transcript-card.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class TranscriptCard {
  readonly transcript = input.required<Transcript>();

  protected readonly clock = (t: Transcript): string => formatClock(t.start);
}
