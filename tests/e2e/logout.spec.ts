import { test, expect } from '@playwright/test';
import { injectPreload, waitForApp, terminalNavigate } from './helpers';

test.describe('Logout flow', () => {
  test.beforeEach(async ({ page }) => {
    await injectPreload(page);
    await page.goto('/');
    await waitForApp(page);
  });

  test('clicking Log out shows logout confirm screen', async ({ page }) => {
    await terminalNavigate(page, 'menu-item-logout');
    await expect(page.locator('[data-testid="logout-confirm-screen"]')).toBeVisible();
  });

  test('cancel button returns to terminal app', async ({ page }) => {
    await terminalNavigate(page, 'menu-item-logout');
    await page.waitForSelector('[data-testid="logout-confirm-screen"]');

    await page.click('[data-testid="logout-cancel-button"]');

    await expect(page.locator('[data-testid="terminal-app"]')).toBeVisible();
    await expect(page.locator('[data-testid="logout-confirm-screen"]')).not.toBeVisible();
  });

  test('Escape from logout confirm returns to terminal app', async ({ page }) => {
    await terminalNavigate(page, 'menu-item-logout');
    await page.waitForSelector('[data-testid="logout-confirm-screen"]');

    await page.keyboard.press('Escape');

    await expect(page.locator('[data-testid="terminal-app"]')).toBeVisible();
    await expect(page.locator('[data-testid="logout-confirm-screen"]')).not.toBeVisible();
  });

  test('keep data & sign out navigates to auth screen', async ({ page }) => {
    await terminalNavigate(page, 'menu-item-logout');
    await page.waitForSelector('[data-testid="logout-confirm-screen"]');

    await page.click('[data-testid="logout-keep-data-button"]');

    await page.waitForSelector('[data-testid="auth-screen"]', { timeout: 5_000 });
    await expect(page.locator('[data-testid="auth-screen"]')).toBeVisible();
    await expect(page.locator('[data-testid="terminal-app"]')).not.toBeVisible();
  });

  test('delete data & sign out navigates to auth screen', async ({ page }) => {
    await terminalNavigate(page, 'menu-item-logout');
    await page.waitForSelector('[data-testid="logout-confirm-screen"]');

    await page.click('[data-testid="logout-delete-data-button"]');

    await page.waitForSelector('[data-testid="auth-screen"]', { timeout: 5_000 });
    await expect(page.locator('[data-testid="auth-screen"]')).toBeVisible();
    await expect(page.locator('[data-testid="terminal-app"]')).not.toBeVisible();
  });

  test('Log out is also accessible from User page', async ({ page }) => {
    await terminalNavigate(page, 'breadcrumb-settings-button', 'menu-item-user');
    await page.waitForSelector('[data-testid="settings-page"]');

    // Settings page header has a Log out button on the right
    await page.locator('button').filter({ hasText: 'Log out' }).click();

    await expect(page.locator('[data-testid="logout-confirm-screen"]')).toBeVisible();
  });
});
