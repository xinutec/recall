import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';

import { SaidBy } from './said-by';

describe('SaidBy', () => {
  async function setup() {
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
    const fixture = TestBed.createComponent(SaidBy);
    fixture.componentRef.setInput('names', ['P', 'D']);
    fixture.componentRef.setInput('known', ['P', 'D', 'Sam']);
    await fixture.whenStable();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any -- the component's private state, reached in a test
    return { fixture, c: fixture.componentInstance as any };
  }

  it('suggests known names matching what was typed', async () => {
    const { c } = await setup();
    c.typed.set('sa');
    expect(c.suggestions()).toEqual(['Sam']);
  });

  it('a chosen name is trimmed, and a blank one is no choice', async () => {
    const { fixture } = await setup();
    const chosen: string[] = [];
    fixture.componentInstance.chosen.subscribe((n) => chosen.push(n));
    const c = fixture.componentInstance as unknown as { choose(n: string): void };
    c.choose('  Dr. Lee ');
    c.choose('   ');
    expect(chosen).toEqual(['Dr. Lee']);
  });
});
