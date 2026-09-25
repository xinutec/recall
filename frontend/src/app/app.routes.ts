import { Routes } from '@angular/router';

import { Check } from './features/check';
import { Labels } from './features/labels';
import { Search } from './features/search';
import { Session } from './features/session';
import { Sessions } from './features/sessions';
import { Timeline } from './features/timeline';

export const routes: Routes = [
  { path: '', pathMatch: 'full', title: 'recall · timeline', component: Timeline },
  { path: 'search', title: 'recall · search', component: Search },
  { path: 'check', title: 'recall · check words', component: Check },
  { path: 'review', redirectTo: 'check' },
  { path: 'labels', title: 'recall · review labels', component: Labels },
  { path: 'sessions', title: 'recall · sessions', component: Sessions },
  { path: 'sessions/:id', title: 'recall · session', component: Session },
  { path: '**', redirectTo: '' },
];
