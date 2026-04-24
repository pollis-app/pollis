import { Text as RNText, TextProps, StyleSheet } from 'react-native';
import { colors, fonts } from '../theme/tokens';

type Weight = 'regular' | 'medium' | 'semibold' | 'bold';

interface Props extends TextProps {
  weight?: Weight;
  size?: number;
  color?: string;
}

export function Text({
  weight = 'regular',
  size = 16,
  color = colors.onSurface,
  style,
  children,
  ...rest
}: Props) {
  return (
    <RNText
      {...rest}
      style={[
        styles.base,
        { fontFamily: fonts[weight], fontSize: size, color },
        style,
      ]}
    >
      {children}
    </RNText>
  );
}

const styles = StyleSheet.create({
  base: {
    includeFontPadding: false,
  },
});
