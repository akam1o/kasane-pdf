//! Predefined simple-font encodings (ISO 32000-1 Annex D):
//! `StandardEncoding` (the built-in encoding of the non-symbolic
//! Standard 14 Type 1 fonts and the default base for Type 1
//! `/Differences`) and `MacRomanEncoding`. `WinAnsiEncoding` lives in
//! `font.rs`. Codes not assigned by Annex D are `None`.

/// StandardEncoding, code → glyph name.
pub(crate) static STANDARD: [Option<&str>; 256] = [
    None,                   // 0
    None,                   // 1
    None,                   // 2
    None,                   // 3
    None,                   // 4
    None,                   // 5
    None,                   // 6
    None,                   // 7
    None,                   // 8
    None,                   // 9
    None,                   // 10
    None,                   // 11
    None,                   // 12
    None,                   // 13
    None,                   // 14
    None,                   // 15
    None,                   // 16
    None,                   // 17
    None,                   // 18
    None,                   // 19
    None,                   // 20
    None,                   // 21
    None,                   // 22
    None,                   // 23
    None,                   // 24
    None,                   // 25
    None,                   // 26
    None,                   // 27
    None,                   // 28
    None,                   // 29
    None,                   // 30
    None,                   // 31
    Some("space"),          // 32
    Some("exclam"),         // 33
    Some("quotedbl"),       // 34
    Some("numbersign"),     // 35
    Some("dollar"),         // 36
    Some("percent"),        // 37
    Some("ampersand"),      // 38
    Some("quoteright"),     // 39
    Some("parenleft"),      // 40
    Some("parenright"),     // 41
    Some("asterisk"),       // 42
    Some("plus"),           // 43
    Some("comma"),          // 44
    Some("hyphen"),         // 45
    Some("period"),         // 46
    Some("slash"),          // 47
    Some("zero"),           // 48
    Some("one"),            // 49
    Some("two"),            // 50
    Some("three"),          // 51
    Some("four"),           // 52
    Some("five"),           // 53
    Some("six"),            // 54
    Some("seven"),          // 55
    Some("eight"),          // 56
    Some("nine"),           // 57
    Some("colon"),          // 58
    Some("semicolon"),      // 59
    Some("less"),           // 60
    Some("equal"),          // 61
    Some("greater"),        // 62
    Some("question"),       // 63
    Some("at"),             // 64
    Some("A"),              // 65
    Some("B"),              // 66
    Some("C"),              // 67
    Some("D"),              // 68
    Some("E"),              // 69
    Some("F"),              // 70
    Some("G"),              // 71
    Some("H"),              // 72
    Some("I"),              // 73
    Some("J"),              // 74
    Some("K"),              // 75
    Some("L"),              // 76
    Some("M"),              // 77
    Some("N"),              // 78
    Some("O"),              // 79
    Some("P"),              // 80
    Some("Q"),              // 81
    Some("R"),              // 82
    Some("S"),              // 83
    Some("T"),              // 84
    Some("U"),              // 85
    Some("V"),              // 86
    Some("W"),              // 87
    Some("X"),              // 88
    Some("Y"),              // 89
    Some("Z"),              // 90
    Some("bracketleft"),    // 91
    Some("backslash"),      // 92
    Some("bracketright"),   // 93
    Some("asciicircum"),    // 94
    Some("underscore"),     // 95
    Some("quoteleft"),      // 96
    Some("a"),              // 97
    Some("b"),              // 98
    Some("c"),              // 99
    Some("d"),              // 100
    Some("e"),              // 101
    Some("f"),              // 102
    Some("g"),              // 103
    Some("h"),              // 104
    Some("i"),              // 105
    Some("j"),              // 106
    Some("k"),              // 107
    Some("l"),              // 108
    Some("m"),              // 109
    Some("n"),              // 110
    Some("o"),              // 111
    Some("p"),              // 112
    Some("q"),              // 113
    Some("r"),              // 114
    Some("s"),              // 115
    Some("t"),              // 116
    Some("u"),              // 117
    Some("v"),              // 118
    Some("w"),              // 119
    Some("x"),              // 120
    Some("y"),              // 121
    Some("z"),              // 122
    Some("braceleft"),      // 123
    Some("bar"),            // 124
    Some("braceright"),     // 125
    Some("asciitilde"),     // 126
    None,                   // 127
    None,                   // 128
    None,                   // 129
    None,                   // 130
    None,                   // 131
    None,                   // 132
    None,                   // 133
    None,                   // 134
    None,                   // 135
    None,                   // 136
    None,                   // 137
    None,                   // 138
    None,                   // 139
    None,                   // 140
    None,                   // 141
    None,                   // 142
    None,                   // 143
    None,                   // 144
    None,                   // 145
    None,                   // 146
    None,                   // 147
    None,                   // 148
    None,                   // 149
    None,                   // 150
    None,                   // 151
    None,                   // 152
    None,                   // 153
    None,                   // 154
    None,                   // 155
    None,                   // 156
    None,                   // 157
    None,                   // 158
    None,                   // 159
    None,                   // 160
    Some("exclamdown"),     // 161
    Some("cent"),           // 162
    Some("sterling"),       // 163
    Some("fraction"),       // 164
    Some("yen"),            // 165
    Some("florin"),         // 166
    Some("section"),        // 167
    Some("currency"),       // 168
    Some("quotesingle"),    // 169
    Some("quotedblleft"),   // 170
    Some("guillemotleft"),  // 171
    Some("guilsinglleft"),  // 172
    Some("guilsinglright"), // 173
    Some("fi"),             // 174
    Some("fl"),             // 175
    None,                   // 176
    Some("endash"),         // 177
    Some("dagger"),         // 178
    Some("daggerdbl"),      // 179
    Some("periodcentered"), // 180
    None,                   // 181
    Some("paragraph"),      // 182
    Some("bullet"),         // 183
    Some("quotesinglbase"), // 184
    Some("quotedblbase"),   // 185
    Some("quotedblright"),  // 186
    Some("guillemotright"), // 187
    Some("ellipsis"),       // 188
    Some("perthousand"),    // 189
    None,                   // 190
    Some("questiondown"),   // 191
    None,                   // 192
    Some("grave"),          // 193
    Some("acute"),          // 194
    Some("circumflex"),     // 195
    Some("tilde"),          // 196
    Some("macron"),         // 197
    Some("breve"),          // 198
    Some("dotaccent"),      // 199
    Some("dieresis"),       // 200
    None,                   // 201
    Some("ring"),           // 202
    Some("cedilla"),        // 203
    None,                   // 204
    Some("hungarumlaut"),   // 205
    Some("ogonek"),         // 206
    Some("caron"),          // 207
    Some("emdash"),         // 208
    None,                   // 209
    None,                   // 210
    None,                   // 211
    None,                   // 212
    None,                   // 213
    None,                   // 214
    None,                   // 215
    None,                   // 216
    None,                   // 217
    None,                   // 218
    None,                   // 219
    None,                   // 220
    None,                   // 221
    None,                   // 222
    None,                   // 223
    None,                   // 224
    Some("AE"),             // 225
    None,                   // 226
    Some("ordfeminine"),    // 227
    None,                   // 228
    None,                   // 229
    None,                   // 230
    None,                   // 231
    Some("Lslash"),         // 232
    Some("Oslash"),         // 233
    Some("OE"),             // 234
    Some("ordmasculine"),   // 235
    None,                   // 236
    None,                   // 237
    None,                   // 238
    None,                   // 239
    None,                   // 240
    Some("ae"),             // 241
    None,                   // 242
    None,                   // 243
    None,                   // 244
    Some("dotlessi"),       // 245
    None,                   // 246
    None,                   // 247
    Some("lslash"),         // 248
    Some("oslash"),         // 249
    Some("oe"),             // 250
    Some("germandbls"),     // 251
    None,                   // 252
    None,                   // 253
    None,                   // 254
    None,                   // 255
];

/// MacRomanEncoding, code → glyph name (Mac OS Roman incl. the maths symbols Annex D leaves unassigned, as pdf.js/poppler do).
pub(crate) static MAC_ROMAN: [Option<&str>; 256] = [
    None,                   // 0
    None,                   // 1
    None,                   // 2
    None,                   // 3
    None,                   // 4
    None,                   // 5
    None,                   // 6
    None,                   // 7
    None,                   // 8
    None,                   // 9
    None,                   // 10
    None,                   // 11
    None,                   // 12
    None,                   // 13
    None,                   // 14
    None,                   // 15
    None,                   // 16
    None,                   // 17
    None,                   // 18
    None,                   // 19
    None,                   // 20
    None,                   // 21
    None,                   // 22
    None,                   // 23
    None,                   // 24
    None,                   // 25
    None,                   // 26
    None,                   // 27
    None,                   // 28
    None,                   // 29
    None,                   // 30
    None,                   // 31
    Some("space"),          // 32
    Some("exclam"),         // 33
    Some("quotedbl"),       // 34
    Some("numbersign"),     // 35
    Some("dollar"),         // 36
    Some("percent"),        // 37
    Some("ampersand"),      // 38
    Some("quotesingle"),    // 39
    Some("parenleft"),      // 40
    Some("parenright"),     // 41
    Some("asterisk"),       // 42
    Some("plus"),           // 43
    Some("comma"),          // 44
    Some("hyphen"),         // 45
    Some("period"),         // 46
    Some("slash"),          // 47
    Some("zero"),           // 48
    Some("one"),            // 49
    Some("two"),            // 50
    Some("three"),          // 51
    Some("four"),           // 52
    Some("five"),           // 53
    Some("six"),            // 54
    Some("seven"),          // 55
    Some("eight"),          // 56
    Some("nine"),           // 57
    Some("colon"),          // 58
    Some("semicolon"),      // 59
    Some("less"),           // 60
    Some("equal"),          // 61
    Some("greater"),        // 62
    Some("question"),       // 63
    Some("at"),             // 64
    Some("A"),              // 65
    Some("B"),              // 66
    Some("C"),              // 67
    Some("D"),              // 68
    Some("E"),              // 69
    Some("F"),              // 70
    Some("G"),              // 71
    Some("H"),              // 72
    Some("I"),              // 73
    Some("J"),              // 74
    Some("K"),              // 75
    Some("L"),              // 76
    Some("M"),              // 77
    Some("N"),              // 78
    Some("O"),              // 79
    Some("P"),              // 80
    Some("Q"),              // 81
    Some("R"),              // 82
    Some("S"),              // 83
    Some("T"),              // 84
    Some("U"),              // 85
    Some("V"),              // 86
    Some("W"),              // 87
    Some("X"),              // 88
    Some("Y"),              // 89
    Some("Z"),              // 90
    Some("bracketleft"),    // 91
    Some("backslash"),      // 92
    Some("bracketright"),   // 93
    Some("asciicircum"),    // 94
    Some("underscore"),     // 95
    Some("grave"),          // 96
    Some("a"),              // 97
    Some("b"),              // 98
    Some("c"),              // 99
    Some("d"),              // 100
    Some("e"),              // 101
    Some("f"),              // 102
    Some("g"),              // 103
    Some("h"),              // 104
    Some("i"),              // 105
    Some("j"),              // 106
    Some("k"),              // 107
    Some("l"),              // 108
    Some("m"),              // 109
    Some("n"),              // 110
    Some("o"),              // 111
    Some("p"),              // 112
    Some("q"),              // 113
    Some("r"),              // 114
    Some("s"),              // 115
    Some("t"),              // 116
    Some("u"),              // 117
    Some("v"),              // 118
    Some("w"),              // 119
    Some("x"),              // 120
    Some("y"),              // 121
    Some("z"),              // 122
    Some("braceleft"),      // 123
    Some("bar"),            // 124
    Some("braceright"),     // 125
    Some("asciitilde"),     // 126
    None,                   // 127
    Some("Adieresis"),      // 128
    Some("Aring"),          // 129
    Some("Ccedilla"),       // 130
    Some("Eacute"),         // 131
    Some("Ntilde"),         // 132
    Some("Odieresis"),      // 133
    Some("Udieresis"),      // 134
    Some("aacute"),         // 135
    Some("agrave"),         // 136
    Some("acircumflex"),    // 137
    Some("adieresis"),      // 138
    Some("atilde"),         // 139
    Some("aring"),          // 140
    Some("ccedilla"),       // 141
    Some("eacute"),         // 142
    Some("egrave"),         // 143
    Some("ecircumflex"),    // 144
    Some("edieresis"),      // 145
    Some("iacute"),         // 146
    Some("igrave"),         // 147
    Some("icircumflex"),    // 148
    Some("idieresis"),      // 149
    Some("ntilde"),         // 150
    Some("oacute"),         // 151
    Some("ograve"),         // 152
    Some("ocircumflex"),    // 153
    Some("odieresis"),      // 154
    Some("otilde"),         // 155
    Some("uacute"),         // 156
    Some("ugrave"),         // 157
    Some("ucircumflex"),    // 158
    Some("udieresis"),      // 159
    Some("dagger"),         // 160
    Some("degree"),         // 161
    Some("cent"),           // 162
    Some("sterling"),       // 163
    Some("section"),        // 164
    Some("bullet"),         // 165
    Some("paragraph"),      // 166
    Some("germandbls"),     // 167
    Some("registered"),     // 168
    Some("copyright"),      // 169
    Some("trademark"),      // 170
    Some("acute"),          // 171
    Some("dieresis"),       // 172
    Some("notequal"),       // 173
    Some("AE"),             // 174
    Some("Oslash"),         // 175
    Some("infinity"),       // 176
    Some("plusminus"),      // 177
    Some("lessequal"),      // 178
    Some("greaterequal"),   // 179
    Some("yen"),            // 180
    Some("mu"),             // 181
    Some("partialdiff"),    // 182
    Some("summation"),      // 183
    Some("product"),        // 184
    Some("pi"),             // 185
    Some("integral"),       // 186
    Some("ordfeminine"),    // 187
    Some("ordmasculine"),   // 188
    Some("Omega"),          // 189
    Some("ae"),             // 190
    Some("oslash"),         // 191
    Some("questiondown"),   // 192
    Some("exclamdown"),     // 193
    Some("logicalnot"),     // 194
    Some("radical"),        // 195
    Some("florin"),         // 196
    Some("approxequal"),    // 197
    Some("Delta"),          // 198
    Some("guillemotleft"),  // 199
    Some("guillemotright"), // 200
    Some("ellipsis"),       // 201
    Some("space"),          // 202
    Some("Agrave"),         // 203
    Some("Atilde"),         // 204
    Some("Otilde"),         // 205
    Some("OE"),             // 206
    Some("oe"),             // 207
    Some("endash"),         // 208
    Some("emdash"),         // 209
    Some("quotedblleft"),   // 210
    Some("quotedblright"),  // 211
    Some("quoteleft"),      // 212
    Some("quoteright"),     // 213
    Some("divide"),         // 214
    Some("lozenge"),        // 215
    Some("ydieresis"),      // 216
    Some("Ydieresis"),      // 217
    Some("fraction"),       // 218
    Some("currency"),       // 219
    Some("guilsinglleft"),  // 220
    Some("guilsinglright"), // 221
    Some("fi"),             // 222
    Some("fl"),             // 223
    Some("daggerdbl"),      // 224
    Some("periodcentered"), // 225
    Some("quotesinglbase"), // 226
    Some("quotedblbase"),   // 227
    Some("perthousand"),    // 228
    Some("Acircumflex"),    // 229
    Some("Ecircumflex"),    // 230
    Some("Aacute"),         // 231
    Some("Edieresis"),      // 232
    Some("Egrave"),         // 233
    Some("Iacute"),         // 234
    Some("Icircumflex"),    // 235
    Some("Idieresis"),      // 236
    Some("Igrave"),         // 237
    Some("Oacute"),         // 238
    Some("Ocircumflex"),    // 239
    Some("apple"),          // 240
    Some("Ograve"),         // 241
    Some("Uacute"),         // 242
    Some("Ucircumflex"),    // 243
    Some("Ugrave"),         // 244
    Some("dotlessi"),       // 245
    Some("circumflex"),     // 246
    Some("tilde"),          // 247
    Some("macron"),         // 248
    Some("breve"),          // 249
    Some("dotaccent"),      // 250
    Some("ring"),           // 251
    Some("cedilla"),        // 252
    Some("hungarumlaut"),   // 253
    Some("ogonek"),         // 254
    Some("caron"),          // 255
];

/// `uniXXXX` / `uXXXX[XX]` glyph names (Adobe Glyph List
/// specification §3) → Unicode scalar; `None` for anything else.
pub(crate) fn uni_name_to_unicode(name: &str) -> Option<u32> {
    let hex = if let Some(h) = name.strip_prefix("uni") {
        if h.len() < 4
            || !h
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase())
        {
            return None;
        }
        &h[..4]
    } else {
        let h = name.strip_prefix('u')?;
        if !(4..=6).contains(&h.len()) || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        h
    };
    let v = u32::from_str_radix(hex, 16).ok()?;
    char::from_u32(v).map(|_| v)
}

/// `gNN` / `glyphNN` / `index NN` / `GNN` names used by subsetters
/// that address TrueType glyphs by index (no AGL meaning).
pub(crate) fn glyph_index_name(name: &str) -> Option<u16> {
    let digits = name
        .strip_prefix("glyph")
        .or_else(|| name.strip_prefix("index"))
        .or_else(|| name.strip_prefix('g'))
        .or_else(|| name.strip_prefix('G'))?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u16>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_and_name_forms() {
        assert_eq!(STANDARD[39], Some("quoteright"));
        assert_eq!(STANDARD[251], Some("germandbls"));
        assert_eq!(MAC_ROMAN[39], Some("quotesingle"));
        assert_eq!(MAC_ROMAN[128], Some("Adieresis"));
        assert_eq!(MAC_ROMAN[255], Some("caron"));
        assert_eq!(uni_name_to_unicode("uni0041"), Some(0x41));
        assert_eq!(uni_name_to_unicode("u1F600"), Some(0x1F600));
        assert_eq!(uni_name_to_unicode("universe"), None);
        assert_eq!(glyph_index_name("g12"), Some(12));
        assert_eq!(glyph_index_name("glyph7"), Some(7));
        assert_eq!(glyph_index_name("gamma"), None);
    }
}
