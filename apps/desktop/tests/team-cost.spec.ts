import { test, expect } from '@playwright/test';
import { illustrativeCallCost, relativeCost, modelCostNote } from '../src/team-models';

test('short cost notes compare identical simulated token volumes', () => {
  expect(illustrativeCallCost('lightning')).toBeCloseTo(.00108, 8);
  expect(illustrativeCallCost('ultra')).toBeCloseTo(.016, 8);
  expect(relativeCost('super')).toBeCloseTo(4.444444, 6);
  expect(relativeCost('ultra')).toBeCloseTo(14.8148148, 6);
  expect(relativeCost('nano')).toBe(1);
  expect(modelCostNote('super')).toBe('(≈ 4,4× le coût de Lightning)');
  expect(modelCostNote('ultra')).toBe('(≈ 14,8× le coût de Lightning)');
  expect(modelCostNote('nano')).toBe('(mêmes tarifs que Lightning)');
});
