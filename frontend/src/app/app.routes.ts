import { Routes } from '@angular/router';

import { Check } from './features/check';
import { Labels } from './features/labels';
import { Search } from './features/search';
import { Session } from './features/session';
import { Sessions } from './features/sessions';
import { Timeline } from './features/timeline';

// Every route but the root says where it sits (@xinutec/ui-scaffold): `top` for a
// main screen the bottom bar reaches, `up` for one drilled into, which the bar
// draws as its arrow.
export const routes: Routes = [
  { path: '', pathMatch: 'full', title: 'recall · timeline', component: Timeline },
  { path: 'search', title: 'recall · search', component: Search, data: { top: true } },
  { path: 'check', title: 'recall · check words', component: Check, data: { top: true } },
  { path: 'review', redirectTo: 'check' },
  { path: 'labels', title: 'recall · review labels', component: Labels, data: { up: '/' } },
  { path: 'sessions', title: 'recall · sessions', component: Sessions, data: { top: true } },
  {
    path: 'sessions/:id',
    title: 'recall · session',
    component: Session,
    data: { up: { path: '/sessions', label: 'Sessions' } },
  },
  { path: '**', redirectTo: '' },
];
