import { Component, inject } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Store } from '../store';

@Component({
  selector: 'sj-settings',
  imports: [FormsModule],
  template: `
    <header class="page-header">
      <div>
        <h1>Settings</h1>
        <p class="muted">Connection options.</p>
      </div>
    </header>

    <section class="card stack">
      <label class="field">
        <span>Name shown on the TV</span>
        <input [ngModel]="store.draft()!.join_name" (ngModelChange)="store.set('join_name', $event)" />
        <small class="muted">Appears as a connected remote in the YouTube app.</small>
      </label>

      <label class="switch-row">
        <div>
          <div>Use system proxy</div>
          <div class="small muted">Honour HTTP(S)_PROXY environment variables.</div>
        </div>
        <input type="checkbox" class="switch" [ngModel]="store.draft()!.use_proxy"
               (ngModelChange)="store.set('use_proxy', $event)" />
      </label>
    </section>

    <section class="card">
      <h2>About</h2>
      <dl class="about">
        <dt>Version</dt><dd>{{ store.meta()?.version }}</dd>
        <dt>Config file</dt><dd class="mono small">{{ store.meta()?.config_path }}</dd>
        <dt>Log file</dt><dd class="mono small">{{ store.meta()?.log_path ?? 'disabled' }}</dd>
      </dl>
      <p class="small muted">
        The config file is shared with the headless <code>skipjack</code> CLI.
      </p>
      <button class="btn ghost" (click)="store.reload()">Reload from disk</button>
    </section>
  `,
})
export class SettingsPage {
  protected readonly store = inject(Store);
}
