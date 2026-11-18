// SPDX-License-Identifier: Apache-2.0
// Types for the converter, so the tests can call it.
export const RENAMED: Readonly<Record<string, string | null>>;
export const PLURAL_SUFFIXES: readonly string[];
export function pluralCategories(tag: string): string[];
export function convertPlaceholders(s: string): string;
export function convertTable(table: Record<string, unknown>, tag: string): Record<string, string>;
export function flatten(obj: unknown, prefix?: string, out?: Record<string, string>): Record<string, string>;
export function unflatten(flat: Record<string, string>): Record<string, unknown>;
export function expectedKeys(sourceFlat: Record<string, string>, tag: string): string[];
export function fillLocale(sourceFlat: Record<string, string>, flat: Record<string, string>, tag: string): Record<string, string>;
export function serialize(flat: Record<string, string>): string;
