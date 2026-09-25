import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  OnDestroy,
} from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MatIconModule } from '@angular/material/icon';

import { Player } from './player';

/** Play or pause one clip through the app's one player. */
@Component({
  selector: 'app-play-button',
  imports: [MatButtonModule, MatIconModule],
  templateUrl: './play-button.html',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class PlayButton implements OnDestroy {
  readonly url = input.required<string>();
  readonly label = input('play');

  private readonly player = inject(Player);
  protected readonly on = computed(() => this.player.playing() === `clip:${this.url()}`);

  protected toggle(): void {
    this.player.toggle(`clip:${this.url()}`, this.player.clip(this.url()));
  }

  ngOnDestroy(): void {
    if (this.on()) this.player.stop();
  }
}
