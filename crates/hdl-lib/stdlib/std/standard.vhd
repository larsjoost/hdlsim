-- std.standard — embedded subset of the IEEE 1076 standard package.
-- Real VHDL source, parsed by hdlsim's own VHDL front-end like any project
-- file (see crates/hdl-lib/src/builtin.rs for how this tree is located).
-- The kernel-primitive types (integer, boolean, ...) are additionally
-- registered directly in the lowerer as a fast path; this file makes the
-- declarations visible to the normal parse/lower pipeline.
package standard is

    type boolean is (false, true);

    type bit is ('0', '1');

    -- 256 character literals: control chars as named literals, printable
    -- ASCII as character literals (apostrophe is ''''), Latin-1 as cXX names.
    type character is (
        nul, soh, stx, etx, eot, enq, ack, bel, bs, ht,
        lf, vt, ff, cr, so, si, dle, dc1, dc2, dc3,
        dc4, nak, syn, etb, can, em, sub, esc, fs, gs,
        rs, us, del, ' ', '!', '"', '#', '$', '%', '&',
        '''', '(', ')', '*', '+', ',', '-', '.', '/', '0',
        '1', '2', '3', '4', '5', '6', '7', '8', '9', ':',
        ';', '<', '=', '>', '?', '@', 'A', 'B', 'C', 'D',
        'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N',
        'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X',
        'Y', 'Z', '[', '\', ']', '^', '_', '`', 'a', 'b',
        'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l',
        'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v',
        'w', 'x', 'y', 'z', '{', '|', '}', '~', c80, c81,
        c82, c83, c84, c85, c86, c87, c88, c89, c8A, c8B,
        c8C, c8D, c8E, c8F, c90, c91, c92, c93, c94, c95,
        c96, c97, c98, c99, c9A, c9B, c9C, c9D, c9E, c9F,
        cA0, cA1, cA2, cA3, cA4, cA5, cA6, cA7, cA8, cA9,
        cAA, cAB, cAC, cAD, cAE, cAF, cB0, cB1, cB2, cB3,
        cB4, cB5, cB6, cB7, cB8, cB9, cBA, cBB, cBC, cBD,
        cBE, cBF, cC0, cC1, cC2, cC3, cC4, cC5, cC6, cC7,
        cC8, cC9, cCA, cCB, cCC, cCD, cCE, cCF, cD0, cD1,
        cD2, cD3, cD4, cD5, cD6, cD7, cD8, cD9, cDA, cDB,
        cDC, cDD, cDE, cDF, cE0, cE1, cE2, cE3, cE4, cE5,
        cE6, cE7, cE8, cE9, cEA, cEB, cEC, cED, cEE, cEF,
        cF0, cF1, cF2, cF3, cF4, cF5, cF6, cF7, cF8, cF9,
        cFA, cFB, cFC, cFD, cFE, cFF
    );

    type string is array (positive range <>) of character;

    type bit_vector is array (natural range <>) of bit;

    type severity_level is (note, warning, error, failure);

    type integer is range -2147483648 to 2147483647;

    subtype natural is integer range 0 to integer'high;

    subtype positive is integer range 1 to integer'high;

    type time is range -9223372036854775808 to 9223372036854775807
        units
            fs;
            ps = 1000 fs;
            ns = 1000 ps;
            us = 1000 ns;
            ms = 1000 us;
            sec = 1000 ms;
            min = 60 sec;
            hr = 60 min;
        end units;

    subtype delay_length is time range 0 fs to time'high;

    type real is range -1.0e308 to 1.0e308;

    -- VHDL-2008: file open kinds/status moved to std.standard.
    type file_open_kind is (read_mode, write_mode, append_mode);
    type file_open_status is (open_ok, status_error, name_error, mode_error);

    -- VHDL-2019 unconstrained vector types.
    type integer_vector is array (natural range <>) of integer;
    type real_vector is array (natural range <>) of real;
    type time_vector is array (natural range <>) of time;
    type boolean_vector is array (natural range <>) of boolean;

    function now return time;

end package standard;
