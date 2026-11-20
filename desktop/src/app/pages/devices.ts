import { Component, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Store } from '../store';

@Component({
  selector: 'sj-devices',
  imports: [FormsModule],
  template: `
    <header class="page-header">
      <div>
        <h1>Devices</h1>
        <p class="muted">TVs, Chromecasts and consoles running the YouTube app.</p>
      </div>
    </header>

    <section class="card">
      <h2>Linked devices</h2>
      @if (devices().length === 0) {
        <p class="muted">No devices yet – add one below.</p>
      }
      @for (d of devices(); track d.screen_id; let i = $index) {
        <div class="row device-row">
          <label class="field grow">
            <span>Name</span>
            <input [ngModel]="d.name" (ngModelChange)="setName(i, $event)" />
          </label>
          <button class="btn ghost danger-text" (click)="remove(i)" title="Remove device">✕</button>
          <div class="mono small muted full">screen id: {{ d.screen_id }}
            @if (d.screen_id.length === 26) {
              <span class="warn-text">· old id format, please pair again</span>
            }
          </div>
        </div>
      }
    </section>

    <div class="two-col">
      <section class="card">
        <h2>Pair with TV code</h2>
        <p class="muted small">
          On the TV open YouTube → Settings → <b>Link with TV code</b> and enter the code shown.
        </p>
        <form class="row" (ngSubmit)="pair()">
          <input class="grow code-input" name="code" placeholder="123 456 789 012" autocomplete="off"
                 [ngModel]="code()" (ngModelChange)="code.set($event)" />
          <button class="btn primary" type="submit" [disabled]="pairing() || !codeOk()">
            {{ pairing() ? 'Pairing…' : 'Pair' }}
          </button>
        </form>
      </section>

      <section class="card">
        <h2>Scan network</h2>
        <p class="muted small">
          Finds devices via DIAL and Chromecast. The YouTube app may briefly open on your TVs.
        </p>
        <button class="btn" (click)="scan()" [disabled]="store.discovering()">
          {{ store.discovering() ? 'Scanning…' : 'Scan for devices' }}
        </button>
        @if (store.discovering()) {
          <div class="progress"></div>
          <p class="muted small">Found {{ store.discovered().length }} so far…</p>
        }
        <ul class="result-list">
          @for (d of store.discovered(); track d.screen_id) {
            <li>
              <div class="grow">
                <div>{{ d.name }}</div>
                <div class="small muted">{{ d.source === 'dial' ? 'DIAL' : 'Chromecast' }}</div>
              </div>
              @if (isAdded(d.screen_id)) {
                <span class="badge">added</span>
              } @else {
                <button class="btn small" (click)="store.addDevice(d.screen_id, d.name)">Add</button>
              }
            </li>
          }
          @if (!store.discovering() && scanned() && store.discovered().length === 0) {
            <li class="muted">No devices found. Make sure the TV is on the same network.</li>
          }
        </ul>
      </section>
    </div>
  `,
})
export class DevicesPage {
  protected readonly store = inject(Store);
  protected readonly code = signal('');
  protected readonly pairing = signal(false);
  protected readonly scanned = signal(false);
  protected readonly devices = computed(() => this.store.draft()?.devices ?? []);
  protected readonly codeOk = computed(() => this.code().replace(/[\s-]/g, '').length >= 6);

  protected scan(): void {
    this.scanned.set(true);
    void this.store.discover();
  }

  protected isAdded(screenId: string): boolean {
    return this.devices().some((d) => d.screen_id === screenId);
  }

  protected setName(index: number, name: string): void {
    this.store.patch((c) => (c.devices[index].name = name));
  }

  protected remove(index: number): void {
    this.store.patch((c) => c.devices.splice(index, 1));
  }

  protected async pair(): Promise<void> {
    this.pairing.set(true);
    const ok = await this.store.pair(this.code());
    this.pairing.set(false);
    if (ok) this.code.set('');
  }
}
