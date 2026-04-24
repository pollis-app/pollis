import { test, expect } from '@playwright/test';
import { injectPreload, waitForApp, terminalNavigate, TEST_USER } from './helpers';

test.describe('User settings', () => {
  test.beforeEach(async ({ page }) => {
    await injectPreload(page, {
      profile: { id: TEST_USER.id, username: 'originalname', phone: '555-0100' },
    });
    await page.goto('/');
    await waitForApp(page);
  });

  test('navigates to settings page', async ({ page }) => {
    await terminalNavigate(page, 'breadcrumb-settings-button', 'menu-item-user');
    await expect(page.locator('[data-testid="settings-page"]')).toBeVisible();
  });

  test('loads existing profile data', async ({ page }) => {
    await terminalNavigate(page, 'breadcrumb-settings-button', 'menu-item-user');
    await page.waitForSelector('[data-testid="settings-page"]');

    await page.waitForFunction(() => {
      const input = document.querySelector('[data-testid="settings-username-input"]') as HTMLInputElement;
      return input && input.value !== '';
    }, { timeout: 5_000 });

    const usernameValue = await page.inputValue('[data-testid="settings-username-input"]');
    expect(usernameValue).toBe('originalname');

    const phoneValue = await page.inputValue('[data-testid="settings-phone-input"]');
    expect(phoneValue).toBe('555-0100');
  });

  test('saves updated username', async ({ page }) => {
    await terminalNavigate(page, 'breadcrumb-settings-button', 'menu-item-user');
    await page.waitForSelector('[data-testid="settings-page"]');

    await page.waitForFunction(() => {
      const input = document.querySelector('[data-testid="settings-username-input"]') as HTMLInputElement;
      return input && input.value !== '';
    }, { timeout: 5_000 });

    await page.fill('[data-testid="settings-username-input"]', 'newusername');
    await page.click('[data-testid="settings-save-button"]');

    await page.waitForSelector('[data-testid="settings-save-success"]', { timeout: 5_000 });
    await expect(page.locator('[data-testid="settings-save-success"]')).toBeVisible();
  });

  test('navigates back from settings via Escape', async ({ page }) => {
    await terminalNavigate(page, 'breadcrumb-settings-button', 'menu-item-user');
    await page.waitForSelector('[data-testid="settings-page"]');

    // First Escape returns to the Settings hub
    await page.keyboard.press('Escape');
    await expect(page.locator('[data-testid="settings-page"]')).not.toBeVisible();
    await expect(page.locator('[data-testid="menu-item-user"]')).toBeVisible();

    // Second Escape returns to Root
    await page.keyboard.press('Escape');
    await expect(page.locator('[data-testid="menu-item-groups"]')).toBeVisible();
  });
});

test.describe('Preferences', () => {
  test('navigates to preferences page', async ({ page }) => {
    await injectPreload(page);
    await page.goto('/');
    await waitForApp(page);

    await terminalNavigate(page, 'breadcrumb-settings-button', 'menu-item-preferences');
    await expect(page.locator('[data-testid="preferences-page"]')).toBeVisible();
  });
});
