import { Component, OnInit, inject, signal } from '@angular/core';
import { Store } from './store';
import { DashboardPage } from './pages/dashboard';
import { DevicesPage } from './pages/devices';
import { AdsPage } from './pages/ads';
import { SettingsPage } from './pages/settings';

type Tab = 'dashboard' | 'devices' | 'ads' | 'settings';

@Component({
  selector: 'sj-root',
  imports: [DashboardPage, DevicesPage, AdsPage, SettingsPage],
  template: `
    <aside class="sidebar">
      <div class="brand">
        <svg viewBox="0 0 64 64" class="logo" aria-hidden="true">
          <defs>
            <linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
              <stop offset="0" stop-color="#0ea5b7" />
              <stop offset="1" stop-color="#2563eb" />
            </linearGradient>
          </defs>
          <rect x="3" y="3" width="58" height="58" rx="14" fill="url(#g)" />
          <path d="M15 19v26l15-13zM30 19v26l15-13z" fill="#fff" />
          <rect x="46" y="19" width="4" height="26" fill="#fff" />
        </svg>
        <div>
          <div class="name">Skipjack</div>
          <div class="tagline">No more ads on your TV</div>
        </div>
      </div>

      <nav>
        @for (item of nav; track item.id) {
          <button class="nav-item" [class.active]="tab() === item.id" (click)="tab.set(item.id)">
            <span class="icon">{{ item.icon }}</span>{{ item.label }}
          </button>
        }
      </nav>

      <div class="service-pill" [class.on]="store.running()">
        <span class="dot"></span>{{ store.running() ? 'Ad blocking active' : 'Stopped' }}
      </div>
    </aside>

    <main class="content">
      @if (store.draft()) {
        @switch (tab()) {
          @case ('dashboard') { <sj-dashboard (navigate)="tab.set($any($event))" /> }
          @case ('devices') { <sj-devices /> }
          @case ('ads') { <sj-ads /> }
          @case ('settings') { <sj-settings /> }
        }
      } @else {
        <div class="loading">Loading…</div>
      }

      @if (store.dirty()) {
        <div class="savebar">
          <span>You have unsaved changes</span>
          <button class="btn ghost" (click)="store.discard()" [disabled]="store.busy()">Discard</button>
          <button class="btn primary" (click)="store.save()" [disabled]="store.busy()">Save</button>
        </div>
      }
    </main>

    @if (store.toast(); as t) {
      <div class="toast" [class.error]="t.error" (click)="store.toast.set(null)">{{ t.text }}</div>
    }
  `,
})
export class App implements OnInit {
  protected readonly store = inject(Store);
  protected readonly tab = signal<Tab>('dashboard');
  protected readonly nav: { id: Tab; label: string; icon: string }[] = [
    { id: 'dashboard', label: 'Dashboard', icon: '◉' },
    { id: 'devices', label: 'Devices', icon: '▭' },
    { id: 'ads', label: 'Ads', icon: '⏭' },
    { id: 'settings', label: 'Settings', icon: '⚙' },
  ];

  ngOnInit(): void {
    void this.store.init();
  }
}
