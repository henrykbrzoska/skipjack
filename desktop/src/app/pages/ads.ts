import { Component, inject } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Store } from '../store';

@Component({
  selector: 'sj-ads',
  imports: [FormsModule],
  template: `
    <header class="page-header">
      <div>
        <h1>Ads</h1>
        <p class="muted">What Skipjack does when YouTube plays an ad on your TV.</p>
      </div>
    </header>

    <section class="card">
      <label class="switch-row">
        <div>
          <div>Skip YouTube ads</div>
          <div class="small muted">Presses “Skip ad” as soon as YouTube reports the ad as skippable.</div>
        </div>
        <input type="checkbox" class="switch" [ngModel]="store.draft()!.skip_ads"
               (ngModelChange)="store.set('skip_ads', $event)" />
      </label>
      <label class="switch-row">
        <div>
          <div>Mute YouTube ads</div>
          <div class="small muted">Mutes the TV while an ad that can't be skipped plays, unmutes afterwards.</div>
        </div>
        <input type="checkbox" class="switch" [ngModel]="store.draft()!.mute_ads"
               (ngModelChange)="store.set('mute_ads', $event)" />
      </label>
      <label class="switch-row">
        <div>
          <div>Autoplay</div>
          <div class="small muted">Keep YouTube's autoplay enabled (or force it off).</div>
        </div>
        <input type="checkbox" class="switch" [ngModel]="store.draft()!.auto_play"
               (ngModelChange)="store.set('auto_play', $event)" />
      </label>
      <label class="switch-row">
        <div>
          <div>Experimental ad escape</div>
          <div class="small muted">On a non-skippable ad, remember the video and its position, press BACK to leave the player and re-open the video there (max 3 tries per session, one per ad).</div>
        </div>
        <input type="checkbox" class="switch" [ngModel]="store.draft()!.experimental_reload_ads"
               (ngModelChange)="store.set('experimental_reload_ads', $event)" />
      </label>
    </section>
  `,
})
export class AdsPage {
  protected readonly store = inject(Store);
}
